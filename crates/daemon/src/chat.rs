//! Chats with agents: the daemon's side of a chat pane.
//!
//! A chat has a terminal's ID and goes in panes as terminals do, but it
//! has no grid: it keeps the conversation as turns (a [`ChatNode`]), and
//! clients draw them. Every change to the turns goes into a journal of
//! [`ChatPatch`]es, which each viewer is sent from where it last was.
//!
//! The agent behind the chat is the server's: a chat asks for what it
//! needs with [`Action`]s, and is told what the agent said. Agents differ
//! in what they send: the chat takes what any of them sends, rather than
//! asking which agent it is, so a new agent that sends the same works too.

use std::collections::{HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use agent_client_protocol_schema::MaybeUndefined;
use agent_client_protocol_schema::v1::{
    ContentBlock, PermissionOption, PermissionOptionKind, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOptions, SessionUpdate, StopReason,
    ToolCallContent, ToolCallLocation, ToolCallStatus, ToolCallUpdate,
};
use clackatoo_protocol::{
    AgentTools, ChatAction, ChatNode, ChatPatch, ChatState, Choice, ChoiceKind, Diff, Item,
    Location, Node, Outcome, Patch, Provider, ProviderModels, Question, Sender, Setting,
    SettingChoice, SettingKind, Size, Switch, TerminalId, TerminalInfo, TerminalStatus, Tool,
    ToolContent, ToolKind, ToolStatus, Turn, TurnEnd, TurnFiles, Usage, View, ViewUpdate,
};
use serde_json::Value;

use crate::agent::Opened;
use crate::agents::AgentSpec;
use crate::handoff::{self, Changed, Digest, Step, StepState};
use crate::handover;

mod limits;
mod originals;
pub(crate) mod queue;
mod relations;
mod resume;

use resume::Held;
mod subagent;

pub(crate) use limits::Limits;
pub(crate) use relations::{Cause, MAX_INBOX, Message, Relations};
pub(crate) use subagent::{Wanted, role_for};

/// The most characters of a title the agent gives a conversation that a
/// chat keeps, for its tab.
const TITLE: usize = 80;

/// How many changes a chat keeps for viewers that are behind. One further
/// behind gets the whole chat again.
const JOURNAL: usize = 4096;

/// About the most a chat's journal keeps, in bytes, so a command's output
/// streamed a piece at a time can't fill it.
const JOURNAL_BYTES: usize = 2 * 1024 * 1024;

/// The most of a tool's text a chat keeps, in bytes: the end of it, as
/// the end of a command's output is where what went wrong is.
const TOOL_TEXT: usize = 32 * 1024;

/// The most of each side of a change to a file a chat keeps, in bytes:
/// the start of it.
const DIFF_TEXT: usize = 256 * 1024;

/// The ID of the chat's own setting for what its agent may do with
/// clackatoo's tools. It's shown with the agent's settings, and kept
/// with the values the user gave them, through upgrades, but never sent
/// to the agent, which has no such setting.
const TOOLS_SETTING: &str = "clackatoo.tools";

/// The values the tools setting takes: each one's ID, what it's called,
/// and what it means.
const TOOLS_VALUES: [(AgentTools, &str, &str, &str); 3] = [
    (
        AgentTools::Ask,
        "ask",
        "Ask",
        "Asks before it runs a command or types into a terminal",
    ),
    (
        AgentTools::Allow,
        "allow",
        "Allow",
        "Runs commands in its own terminals without asking",
    ),
    (
        AgentTools::Off,
        "off",
        "Off",
        "None of clackatoo's tools, from the next session on",
    ),
];

/// The ID a chat keeps, with the values chosen, whether its agent keeps
/// Claude Code's own Task tool, so that it lasts through upgrades and
/// the session is opened again the same way. It's chosen when the chat
/// starts, and never shown or sent to the agent as a setting: Claude
/// Code reads it only as a session opens.
const TASK_SETTING: &str = "clackatoo.task";

/// The ID a chat started as a profile keeps, with the values chosen, the
/// profile's name under (see [`crate::profiles`]).
const PROFILE_SETTING: &str = "clackatoo.profile";

/// The ID a chat keeps, with the values chosen, what it may do beyond its
/// agent's own settings under, by [`Limits`](crate::profiles::Limits)'s
/// names, so each session opens with them again.
const LIMITS_SETTING: &str = "clackatoo.limits";

/// The ID of the chat's own setting for whether its agent gets tools that
/// drive clackatoo's browser (see [`crate::browser`]). Shown with the
/// agent's settings, as the tools setting is, and never sent to the
/// agent.
const BROWSER_SETTING: &str = "clackatoo.browser";

/// The values the browser setting takes: each one's ID, what it's called,
/// and what it means.
const BROWSER_VALUES: [(bool, &str, &str, &str); 2] = [
    (
        false,
        "off",
        "Off",
        "No browser: the agent can't open web pages",
    ),
    (
        true,
        "on",
        "On",
        "Drives a browser of clackatoo's, in its own profile, asking before the first page",
    ),
];

/// The ID a chat keeps, with the values chosen, once the user has let its
/// agent open pages in the browser, so it isn't asked again, even after
/// an upgrade. Never shown, nor sent to the agent.
const BROWSING_SETTING: &str = "clackatoo.browsing";

/// Whether the setting with `id` is the chat's own, kept whichever agent
/// it talks to, rather than one of an agent's.
fn chat_wide(id: &str) -> bool {
    [
        TOOLS_SETTING,
        TASK_SETTING,
        PROFILE_SETTING,
        LIMITS_SETTING,
        BROWSER_SETTING,
        BROWSING_SETTING,
    ]
    .contains(&id)
}

/// Whether the setting with `id` is one the chat shows of its own, after
/// the agent's.
fn shown_own(id: &str) -> bool {
    id == TOOLS_SETTING || id == BROWSER_SETTING
}

/// The most questions a chat keeps waiting. Past it, a tool's question is
/// refused rather than queued.
pub(crate) const MAX_QUESTIONS: usize = 16;

/// The answers to a question about one of clackatoo's tools: its ID, what
/// it's called, and what it means.
const TOOL_CHOICES: [(&str, &str, ChoiceKind); 3] = [
    ("allow", "Allow", ChoiceKind::AllowOnce),
    ("always", "Always allow", ChoiceKind::AllowAlways),
    ("reject", "Reject", ChoiceKind::RejectOnce),
];

/// The answers to a question about the agent messaging or starting
/// another chat. "Always" is for this chat and that one alone: it never
/// changes the chat's Tools setting, so no chat gains, through another,
/// what its own setting doesn't give it.
const TALK_CHOICES: [(&str, &str, ChoiceKind); 3] = [
    ("allow", "Allow", ChoiceKind::AllowOnce),
    (
        "pair",
        "Always allow for that chat",
        ChoiceKind::AllowAlways,
    ),
    ("reject", "Reject", ChoiceKind::RejectOnce),
];

/// The answers to the question asked before an agent first opens a page in
/// the browser. Allowing is for the chat, from then on: it's the first
/// page that's asked about, not each.
const BROWSER_CHOICES: [(&str, &str, ChoiceKind); 2] = [
    ("browse", "Allow for this chat", ChoiceKind::AllowAlways),
    ("reject", "Reject", ChoiceKind::RejectOnce),
];

/// Which answers a tool's question offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choices {
    /// Allow, Always allow, which lets the agent run commands in its own
    /// terminals, and Reject.
    Tool,
    /// Allow, Always allow for the chat it's about, and Reject.
    Talk,
    /// Allow for this chat, which lets its agent open pages in the
    /// browser from then on, and Reject.
    Browser,
}

/// A message kept intact while the agent opens or reloads its session.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Prompt {
    pub(crate) text: String,
    pub(crate) images: Vec<clackatoo_protocol::ChatImage>,
}

impl From<String> for Prompt {
    fn from(text: String) -> Self {
        Self {
            text,
            images: Vec::new(),
        }
    }
}
impl From<&str> for Prompt {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}
impl Prompt {
    pub(super) fn shown(&self) -> String {
        let mut shown = self.text.clone();
        for image in &self.images {
            if !shown.is_empty() {
                shown.push_str("\n\n");
            }
            shown.push('[');
            shown.push_str(&image.label());
            shown.push(']');
        }
        shown
    }
}

/// What a chat asks the server to do.
#[derive(Debug, PartialEq)]
pub(crate) enum Action {
    /// Send the agent this message.
    Prompt(Prompt),
    /// Send the agent this message, after `role`, which says what it's
    /// started as: the first message of a chat started as a profile.
    Briefed { role: String, text: Prompt },
    /// Send the agent this message, after a digest of the turns from
    /// `from` up to `to`, which its session hasn't seen, as the chat was
    /// with another agent then.
    Carry {
        text: Prompt,
        from: usize,
        to: usize,
    },
    /// Get the chat a session: its own again, or a new one.
    Connect,
    /// Stop the agent answering.
    Cancel,
    /// Answer the agent's question: with the option picked, or none.
    Answer {
        request: Value,
        option: Option<String>,
    },
    /// Let a call to one of clackatoo's tools go ahead, numbered as the
    /// server numbered it, or refuse it; with `pair`, a call about another
    /// chat goes ahead without asking from now on, for that chat.
    Permit { call: u64, allow: bool, pair: bool },
    /// Change one of the agent's settings for the chat.
    Set { setting: String, value: String },
    /// Install the chat's agent, as the user agreed to.
    Install,
    /// The chat's Browser setting went on, or off: give the agent the
    /// browser's tools, or take them away and close the chat's page.
    Browser(bool),
    /// The chat no longer waits for the session it asked for: one made
    /// after all is to be closed, unseen.
    Abandon,
    /// The agent is slow to open the chat's session: start it again, and
    /// ask again.
    Retry,
}

/// The answer to an install offer that installs.
pub(crate) const INSTALL: &str = "install";

/// Files a tool changed on disk itself, as the agent says once it has,
/// or, while it runs, files it's about to change.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Wrote {
    /// Whether the tool has finished: if not, it names files it's going
    /// to change, which it hasn't yet, or not all.
    pub(crate) done: bool,
    /// The turn it's in.
    pub(crate) turn: usize,
    /// Its ID.
    pub(crate) tool: String,
    /// Where it is among its turn's items, so its changes can be put in
    /// the order the agent made them.
    pub(crate) order: usize,
    /// The files, as the agent names them: absolute, or from the chat's
    /// working directory.
    pub(crate) paths: Vec<String>,
    /// What it says it changed in them, as last said, whole: the changes
    /// to check the files against.
    pub(crate) diffs: Vec<RawDiff>,
}

/// A change a tool says it made to a file: `old` replaced by `new`, as
/// the agent gives it, with no line cut. `old` is `None` for a file the
/// tool made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawDiff {
    pub(crate) path: String,
    pub(crate) old: Option<String>,
    pub(crate) new: String,
}

/// Whether a chat's session is open in the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
    Closed,
    /// The server has been asked to open it.
    Opening,
    Open,
    /// It was let go while idle, and the agent hasn't said it has ended:
    /// a message waits for that before it's opened again, so the end
    /// can't come after, and take the session it opened with it.
    Closing,
}

/// A chat with an agent.
#[derive(Debug)]
pub(crate) struct HostedChat {
    /// Only outstanding workers hold strong receipts. Replaced results revoke them.
    image_receipts: Vec<(String, std::sync::Weak<()>)>,
    pub(crate) originals: crate::tool_images::Originals,
    id: TerminalId,
    /// The size of the pane it's in, for listing.
    size: Size,
    title: String,
    cwd: String,
    /// `cwd` as people write it, from `~` under the home folder, which
    /// its view carries for windows to show: a new chat says which
    /// folder its agent works in. Set by [`show_cwd`](Self::show_cwd)
    /// once the server has it; until then, `cwd` as it is.
    shown_cwd: Option<String>,
    /// The agent's name, which the server finds it by.
    agent: String,
    /// The agent's session, once it has made one.
    session: Option<String>,
    /// Whether the session is open in the agent now. It isn't before it's
    /// made, after an upgrade, or once the agent has gone.
    link: Link,
    /// A message sent while there's no session, to go once there is.
    queued: Option<Prompt>,
    /// Whether the notice says the agent is slow to open the session: it
    /// goes once the session opens, or isn't waited for.
    slow: bool,
    /// Where it is in loading a session its agent kept, if it goes on
    /// with one (see `resume.rs`). Not handed over: after an upgrade, the
    /// session is resumed.
    told: resume::Told,
    node: ChatNode,
    /// The questions waiting for answers, the first shown: the agent may
    /// ask about several tools at once, and clackatoo's own tools ask
    /// too.
    asking: VecDeque<(Asker, Question)>,
    /// The number the next question gets.
    next_question: u64,
    /// The values the user gave settings, by their IDs, which the chat
    /// asks for again whenever its session opens: the agent forgets them
    /// when it stops.
    chosen: Vec<(String, String)>,
    /// The mode a new chat asks for when its session first opens.
    starting_mode: Option<String>,
    journal: Journal,
    /// What each tool of the turn being answered last said it changed in
    /// files, by its ID.
    diffs: Vec<(String, Vec<RawDiff>)>,
    /// Where it stands among the chats agents talk in.
    relations: Relations,
    /// How many of the turns the session has seen, from the first: those
    /// after went to other agents, and it hears of them with its next
    /// message.
    known: usize,
    /// The sessions the chat had with other agents, to go back to.
    parked: Vec<Parked>,
    /// About the most tokens a digest of the conversation takes, for an
    /// agent the chat moves to.
    budget: usize,
    /// The agent's plan, as it last gave it, for a digest to tell the
    /// next agent.
    plan: Vec<Step>,
    /// What the chat moved from, until the new agent's session opens and
    /// its settings can be set to match.
    carry: Option<Carry>,
    /// The agent that gave the plan, as it's shown.
    plan_by: Option<String>,
    /// How many turns the session will have seen once the message sent
    /// with a digest is answered: until it is, without failing, the next
    /// message carries the digest again.
    heard: Option<usize>,
    /// What it's started as, to go before its first message, if it was
    /// started as a profile with a role.
    role: Option<String>,
    /// The settings it was started wanting, to choose as its first
    /// session opens.
    wanted: Option<Wanted>,
    /// How many of its turns the chat that started it has had the answer
    /// to, through `wait_for_chat(s)` or `read_chat`: a chat whose last
    /// answer nobody has had is never closed to make room. Kept through
    /// `&self`, as answers are read; not handed over, so after an upgrade
    /// it's none, which errs on keeping chats.
    told_parent: std::cell::Cell<usize>,
    /// The tool calls whose input named clackatoo's MCP server as they
    /// began, the latest few: a question about one is about our own tool,
    /// whatever it's titled, and the agent can't make a command's input
    /// say so.
    own_tools: VecDeque<String>,
    /// Its agent's account's limits, as the daemon last heard them from
    /// any of that agent's chats, shown with the context it fills. Not
    /// handed over: after an upgrade they come again with the next that
    /// changes.
    limits: Limits,
    user_queue: queue::Queue,
}

/// A session a chat had with another agent, which it can go back to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parked {
    /// The agent's name.
    pub(crate) agent: String,
    /// The agent as it's shown.
    pub(crate) title: String,
    pub(crate) session: Option<String>,
    /// How many of the chat's turns it saw.
    pub(crate) known: usize,
    /// The values the user gave its settings.
    pub(crate) chosen: Vec<(String, String)>,
    /// Its settings, as it last gave them.
    pub(crate) settings: Vec<Setting>,
}

/// What a chat moved from, to set the new agent's settings to match.
#[derive(Debug)]
struct Carry {
    /// The turn that shows the move.
    turn: usize,
    /// The model asked for, by its value.
    model: Option<String>,
    /// The mode and effort it had, to give the new agent the same, where
    /// it offers them. Not for a session it goes back to, which keeps its
    /// own.
    mode: Option<Setting>,
    effort: Option<Setting>,
}

/// Who asked a question in a chat, and so gets the answer.
#[derive(Clone, Debug, PartialEq)]
enum Asker {
    /// The agent, in the request with this JSON-RPC ID.
    Agent(Value),
    /// One of clackatoo's tools the agent called, by the server's number
    /// for the call.
    Tool(u64),
    /// The daemon: whether to install the agent, which the chat waits
    /// for.
    Install,
    /// The daemon: whether to install the agent, which runs another way
    /// meanwhile, so nothing waits for the answer.
    Suggest,
    /// The daemon: what to do about a session the agent is slow to open.
    Slow,
}

/// The answers to the question asked when the agent is slow to open a
/// chat's session: its ID, what it's called, and what it means. Keeping
/// waiting takes the question away, and the notice stays; the session
/// may still come. It's what Esc picks, as a refusal of this once, and
/// none is made the one to press: Retry starts again a process every
/// chat with the agent shares, and Stop gives up.
const SLOW_CHOICES: [(&str, &str, ChoiceKind); 3] = [
    ("wait", "Keep waiting", ChoiceKind::RejectOnce),
    ("retry", "Retry", ChoiceKind::AllowOnce),
    ("stop", "Stop", ChoiceKind::RejectAlways),
];

/// The changes made to a chat, numbered, for viewers to catch up with.
#[derive(Debug, Default)]
struct Journal {
    /// The number of the first change kept.
    first: u64,
    patches: VecDeque<ChatPatch>,
    /// About how much the changes kept take, in bytes.
    bytes: usize,
}

impl Journal {
    /// Keeps `patch`, and lets the oldest go past the journal's bounds, of
    /// changes and of bytes: a viewer that far behind gets the chat whole.
    fn push(&mut self, patch: ChatPatch) {
        self.bytes += weight(&patch);
        self.patches.push_back(patch);
        while self.patches.len() > 1 && (self.patches.len() > JOURNAL || self.bytes > JOURNAL_BYTES)
        {
            let gone = self.patches.pop_front().expect("more than one");
            self.bytes -= weight(&gone);
            self.first += 1;
        }
    }

    /// The number the next change will get.
    fn end(&self) -> u64 {
        self.first + self.patches.len() as u64
    }
}

/// What a viewer was last sent of a chat.
#[derive(Debug, Default)]
pub(crate) struct ChatSent {
    /// The number of the next change it hasn't had, once it has had the
    /// chat whole.
    next: Option<u64>,
    title: String,
}

impl HostedChat {
    /// A chat with `agent`, in `cwd`, not yet with a session: the server
    /// is to get it one. It's named after the agent until the agent names
    /// the conversation. Its agent may do what `tools` says with
    /// clackatoo's tools, and keeps Claude Code's Task tool if `task`.
    pub(crate) fn new(
        id: TerminalId,
        size: Size,
        cwd: String,
        agent: &AgentSpec,
        tools: AgentTools,
        task: bool,
        browser: bool,
    ) -> Self {
        Self {
            image_receipts: Vec::new(),
            originals: crate::tool_images::Originals::default(),
            id,
            size,
            title: agent.title.clone(),
            cwd,
            shown_cwd: None,
            agent: agent.name.clone(),
            session: None,
            link: Link::Opening,
            queued: None,
            slow: false,
            told: resume::Told::default(),
            node: ChatNode {
                agent: agent.title.clone(),
                settings: vec![tools_setting(tools), browser_setting(browser)],
                ..ChatNode::default()
            },
            asking: VecDeque::new(),
            next_question: 0,
            chosen: vec![
                (TOOLS_SETTING.to_owned(), tools_value(tools).to_owned()),
                (
                    TASK_SETTING.to_owned(),
                    if task { "on" } else { "off" }.to_owned(),
                ),
                (
                    BROWSER_SETTING.to_owned(),
                    if browser { "on" } else { "off" }.to_owned(),
                ),
            ],
            starting_mode: agent.starting_mode.clone(),
            journal: Journal::default(),
            diffs: Vec::new(),
            relations: Relations::default(),
            known: 0,
            parked: Vec::new(),
            budget: handoff::BUDGET,
            plan: Vec::new(),
            carry: None,
            plan_by: None,
            heard: None,
            role: None,
            wanted: None,
            told_parent: std::cell::Cell::new(0),
            own_tools: VecDeque::new(),
            limits: Limits::default(),
            user_queue: queue::Queue::default(),
        }
    }

    /// Hands the chat over to the next build. The agent doesn't go with
    /// it: the next build opens the session again when it's next needed,
    /// and an answer under way is cut off.
    pub(crate) fn hand_over(mut self) -> handover::Chat {
        self.hand_over_loading();
        if self.answering() {
            self.end_turn(Outcome::Failed("cut off by an upgrade".to_owned()));
        }
        let failure = match self.node.state {
            ChatState::Failed(why) => Some(why),
            _ => None,
        };
        let senders: Vec<(usize, &Sender)> = (self.node.turns.iter().enumerate())
            .filter_map(|(index, turn)| Some((index, turn.from.as_ref()?)))
            .collect();
        let relations = self.relations.hand_over(&senders);
        let switches = (self.node.turns.iter().enumerate())
            .filter_map(|(turn, each)| {
                let switch = each.switch.clone()?;
                Some(handover::ChatSwitch {
                    turn: turn as u64,
                    from: switch.from,
                    to: switch.to,
                    model: switch.model,
                    resumed: switch.resumed,
                    notes: switch.notes,
                })
            })
            .collect();
        handover::Chat {
            id: self.id.0,
            cols: self.size.cols(),
            rows: self.size.rows(),
            title: self.title,
            cwd: self.cwd,
            session: self.session,
            failure,
            turns: self.node.turns.into_iter().map(turn_to_handover).collect(),
            // The chat's own setting is kept with the values chosen.
            settings: (self.node.settings.into_iter())
                .filter(|setting| !shown_own(&setting.id))
                .map(setting_to_handover)
                .collect(),
            chosen: self.chosen,
            agent: self.agent,
            relations,
            known: self.known as u64,
            parked: (self.parked.into_iter())
                .map(|parked| handover::ParkedSession {
                    agent: parked.agent,
                    title: parked.title,
                    session: parked.session,
                    known: parked.known as u64,
                    chosen: parked.chosen,
                    settings: (parked.settings.into_iter())
                        .map(setting_to_handover)
                        .collect(),
                })
                .collect(),
            switches,
            budget: u32::try_from(self.budget).unwrap_or(u32::MAX),
            plan_by: self.plan_by,
            plan: (self.plan.into_iter())
                .map(|step| handover::PlanStep {
                    content: step.content,
                    state: match step.state {
                        StepState::ToDo => handover::StepState::ToDo,
                        StepState::Doing => handover::StepState::Doing,
                        StepState::Done => handover::StepState::Done,
                    },
                })
                .collect(),
        }
    }

    /// A chat the last build handed over, with `agent`, which is what
    /// its name is now.
    pub(crate) fn resume(chat: handover::Chat, agent: &AgentSpec) -> Self {
        let size = Size::new(chat.cols, chat.rows).unwrap_or(Size::new(80, 24).expect("valid"));
        let mut resumed = Self::new(
            TerminalId(chat.id),
            size,
            chat.cwd,
            agent,
            AgentTools::Ask,
            true,
            false,
        );
        resumed.link = Link::Closed;
        resumed.title = chat.title;
        resumed.session = chat.session;
        resumed.node.turns = chat.turns.into_iter().map(turn_from_handover).collect();
        for switch in chat.switches {
            let at = usize::try_from(switch.turn).unwrap_or(usize::MAX);
            if let Some(turn) = resumed.node.turns.get_mut(at) {
                turn.switch = Some(Switch {
                    from: switch.from,
                    to: switch.to,
                    model: switch.model,
                    resumed: switch.resumed,
                    notes: switch.notes,
                });
            }
        }
        let count = |known: u64| usize::try_from(known).unwrap_or(usize::MAX);
        resumed.known = count(chat.known).min(resumed.node.turns.len());
        resumed.parked = (chat.parked.into_iter())
            .map(|parked| Parked {
                agent: parked.agent,
                title: parked.title,
                session: parked.session,
                known: count(parked.known),
                chosen: parked.chosen,
                settings: (parked.settings.into_iter())
                    .map(setting_from_handover)
                    .collect(),
            })
            .collect();
        resumed.budget = usize::try_from(chat.budget).unwrap_or(handoff::BUDGET);
        resumed.plan_by = chat.plan_by;
        resumed.plan = (chat.plan.into_iter())
            .map(|step| Step {
                content: step.content,
                state: match step.state {
                    handover::StepState::ToDo => StepState::ToDo,
                    handover::StepState::Doing => StepState::Doing,
                    handover::StepState::Done => StepState::Done,
                },
            })
            .collect();
        resumed.node.settings = (chat.settings.into_iter())
            .map(setting_from_handover)
            .collect();
        resumed.chosen = chat.chosen;
        let (relations, senders) = Relations::resume(chat.relations);
        resumed.relations = relations;
        for (index, sender) in senders {
            if let Some(turn) = resumed.node.turns.get_mut(index) {
                turn.from = Some(sender);
            }
        }
        (resumed.node.settings).push(tools_setting(resumed.tools()));
        (resumed.node.settings).push(browser_setting(resumed.browser()));
        resumed.starting_mode = None;
        resumed.node.state = chat.failure.map_or(ChatState::Ready, ChatState::Failed);
        resumed
    }

    pub(crate) fn info(&self) -> TerminalInfo {
        TerminalInfo {
            id: self.id,
            pid: None,
            size: self.size,
            title: self.title.clone(),
            status: TerminalStatus::Running,
            cwd: Some(self.cwd.clone()),
            command: None,
        }
    }

    pub(crate) fn working_directory(&self) -> &str {
        &self.cwd
    }

    /// The agent's name.
    pub(crate) fn agent(&self) -> &str {
        &self.agent
    }

    /// The size of the pane it's in.
    pub(crate) fn size(&self) -> Size {
        self.size
    }

    /// What its agent may do with clackatoo's tools. A chat from before
    /// they could be turned off may use them, asking first.
    pub(crate) fn tools(&self) -> AgentTools {
        let value =
            (self.chosen.iter()).find_map(|(id, value)| (id == TOOLS_SETTING).then_some(value));
        match value.map(String::as_str) {
            Some("off") => AgentTools::Off,
            Some("allow") => AgentTools::Allow,
            _ => AgentTools::Ask,
        }
    }

    /// Whether its agent keeps Claude Code's own Task tool. A chat from
    /// before it could be taken away keeps it.
    pub(crate) fn task(&self) -> bool {
        !(self.chosen.iter()).any(|(id, value)| id == TASK_SETTING && value == "off")
    }

    /// Whether its agent gets tools that drive clackatoo's browser. A chat
    /// from before there was one doesn't.
    pub(crate) fn browser(&self) -> bool {
        (self.chosen.iter()).any(|(id, value)| id == BROWSER_SETTING && value == "on")
    }

    /// Whether the user has let its agent open pages in the browser.
    pub(crate) fn browsing(&self) -> bool {
        (self.chosen.iter()).any(|(id, value)| id == BROWSING_SETTING && value == "yes")
    }

    /// Gives its agent the browser's tools, or takes them away, from now
    /// on. Taking them away forgets that the user let it open pages.
    fn set_browser(&mut self, browser: bool) {
        let value = if browser { "on" } else { "off" };
        self.chosen.retain(|(id, _)| id != BROWSER_SETTING);
        (self.chosen).push((BROWSER_SETTING.to_owned(), value.to_owned()));
        if !browser {
            self.chosen.retain(|(id, _)| id != BROWSING_SETTING);
        }
        self.set_value(BROWSER_SETTING, value);
    }

    /// Lets its agent do what `tools` says with clackatoo's tools, from
    /// now on.
    fn set_tools(&mut self, tools: AgentTools) {
        self.chosen.retain(|(id, _)| id != TOOLS_SETTING);
        (self.chosen).push((TOOLS_SETTING.to_owned(), tools_value(tools).to_owned()));
        self.set_value(TOOLS_SETTING, tools_value(tools));
    }

    /// The sessions it had with other agents.
    pub(crate) fn parked(&self) -> &[Parked] {
        &self.parked
    }

    /// The agent's session, if it has made one.
    pub(crate) fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Has its view say the folder it works in as `shown`: its working
    /// directory as people write it.
    pub(crate) fn show_cwd(&mut self, shown: String) {
        self.shown_cwd = Some(shown);
    }

    pub(crate) fn view(&self) -> View {
        View {
            title: self.title.clone(),
            cwd: Some(self.shown_cwd.clone().unwrap_or_else(|| self.cwd.clone())),
            root: Node::Chat(Box::new(self.node.clone())),
        }
    }

    pub(crate) fn resize(&mut self, size: Size) {
        self.size = size;
    }

    /// What brings a viewer from what it was last sent to the chat now:
    /// the changes since, or the chat whole if it's had none or is too far
    /// behind. `None` if nothing changed.
    pub(crate) fn catch_up(&self, sent: &mut ChatSent) -> Option<ViewUpdate> {
        let end = self.journal.end();
        let Some(next) = sent.next.filter(|&next| next >= self.journal.first) else {
            sent.next = Some(end);
            self.title.clone_into(&mut sent.title);
            return Some(ViewUpdate::Snapshot(self.view()));
        };
        let mut patches = Vec::new();
        if self.title != sent.title {
            self.title.clone_into(&mut sent.title);
            patches.push(Patch::Title(self.title.clone()));
        }
        let skip = usize::try_from(next - self.journal.first).expect("the journal is short");
        let mut chat: Vec<Option<ChatPatch>> = Vec::new();
        // Where each item's first whole value in this update is, which a
        // later one takes the place of.
        let mut items: HashMap<(u32, u32), usize> = HashMap::new();
        for patch in self.journal.patches.iter().skip(skip) {
            match patch {
                ChatPatch::Turn { index, .. } => items.retain(|(turn, _), _| turn != index),
                ChatPatch::Item { turn, item, .. } => {
                    if let Some(&at) = items.get(&(*turn, *item)) {
                        // What was added to it since is in this value.
                        for later in &mut chat[at + 1..] {
                            if later
                                .as_ref()
                                .is_some_and(|later| adds_to(later, *turn, *item))
                            {
                                *later = None;
                            }
                        }
                        chat[at] = Some(patch.clone());
                        continue;
                    }
                    items.insert((*turn, *item), chat.len());
                }
                _ => {}
            }
            // Pieces of the same words, or output, streamed, go as one.
            if let Some(Some(last)) = chat.last_mut()
                && merge(last, patch)
            {
                continue;
            }
            chat.push(Some(patch.clone()));
        }
        patches.extend(chat.into_iter().flatten().map(Patch::Chat));
        sent.next = Some(end);
        (!patches.is_empty()).then_some(ViewUpdate::Patch(patches))
    }

    // --- What the user asks -------------------------------------------------

    /// Takes what a client asks of the chat, and returns what the server
    /// must do about it.
    pub(crate) fn act(&mut self, action: ChatAction) -> Vec<Action> {
        let mut actions = Vec::new();
        match action {
            ChatAction::Send(text) => self.send(text, &mut actions),
            ChatAction::SendImages { text, images } => {
                self.send_prompt(Prompt { text, images }, &mut actions);
            }
            ChatAction::Stop => self.stop(&mut actions),
            // Only an answer to the question shown counts: one sent twice,
            // or as the question was taken back, isn't the next one's.
            ChatAction::Answer { question, option } => {
                if (self.asking.front()).is_some_and(|(_, asked)| asked.id == question)
                    && let Some((asker, _)) = self.asking.pop_front()
                {
                    self.show_question();
                    if asker == Asker::Slow {
                        self.slow_answered(option.as_deref(), &mut actions);
                    } else {
                        actions.extend(self.answer(asker, option));
                    }
                }
            }
            // The chat's own: the agent never hears of it.
            ChatAction::Set { setting, value } if setting == TOOLS_SETTING => {
                if let Some(&(tools, ..)) = (TOOLS_VALUES.iter()).find(|(_, id, ..)| *id == value) {
                    self.set_tools(tools);
                }
            }
            ChatAction::Set { setting, value } if setting == BROWSER_SETTING => {
                if let Some(&(browser, ..)) =
                    (BROWSER_VALUES.iter()).find(|(_, id, ..)| *id == value)
                    && browser != self.browser()
                {
                    self.set_browser(browser);
                    actions.push(Action::Browser(browser));
                }
            }
            // The server's to do: they're about files, or other agents.
            ChatAction::Undo { .. }
            | ChatAction::Redo { .. }
            | ChatAction::Switch { .. }
            | ChatAction::SendTo { .. }
            | ChatAction::Submit { .. }
            | ChatAction::RemoveQueued { .. }
            | ChatAction::SendQueued { .. }
            | ChatAction::RefreshUsage { .. }
            | ChatAction::LearnModels
            | ChatAction::Open { .. } => {}
            // Only a value the agent offers is kept, and asked for.
            ChatAction::Set { setting, value } => {
                if self.set_value(&setting, &value) {
                    self.chosen.retain(|(id, _)| *id != setting);
                    self.chosen.push((setting.clone(), value.clone()));
                    if self.link == Link::Open {
                        actions.push(Action::Set { setting, value });
                    }
                }
            }
        }
        actions
    }

    /// Gives setting `id` the value `value`, as far as the chat's view
    /// goes. Returns whether it has that setting, with that choice.
    fn set_value(&mut self, id: &str, value: &str) -> bool {
        let Some(setting) = self
            .node
            .settings
            .iter_mut()
            .find(|setting| setting.id == id)
        else {
            return false;
        };
        if !setting.choices.iter().any(|choice| choice.value == value) {
            return false;
        }
        if setting.value != value {
            value.clone_into(&mut setting.value);
            self.journal
                .push(ChatPatch::Settings(self.node.settings.clone()));
        }
        true
    }

    /// Sends a message, if there is one and the agent isn't still
    /// answering the last.
    fn send(&mut self, text: String, actions: &mut Vec<Action>) {
        self.send_prompt(text.into(), actions);
    }

    fn send_prompt(&mut self, prompt: Prompt, actions: &mut Vec<Action>) {
        if (prompt.text.trim().is_empty() && prompt.images.is_empty())
            || self.user_queue.native.is_some()
            || self.answering() && !self.loading()
        {
            return;
        }
        if prompt.images.len() > clackatoo_protocol::ChatImage::MAX_COUNT
            || prompt.images.iter().any(|image| !image.valid())
            || prompt
                .images
                .iter()
                .map(|image| image.data.len())
                .sum::<usize>()
                > clackatoo_protocol::ChatImage::MAX_DATA
        {
            self.set_notice(Some(
                "Images are too large: paste up to four images, 8 MiB encoded in total.".into(),
            ));
            return;
        }
        if self.hold_while_loading(&prompt, actions) {
            return;
        }
        // The user's own: what agents do of it comes of this turn.
        self.relations.hops = 0;
        let turn = u32::try_from(self.node.turns.len()).unwrap_or(u32::MAX);
        self.relations.cause = Some((self.id, turn));
        self.begin(prompt.shown(), prompt, None, actions);
    }

    /// Starts a turn: shows `shown`, sent by `from` if another chat's
    /// agent sent it, and sends the agent `prompt`.
    fn begin(
        &mut self,
        shown: String,
        prompt: Prompt,
        from: Option<Sender>,
        actions: &mut Vec<Action>,
    ) {
        // A session slow to open still is: what says so stays until it
        // comes, or isn't waited for.
        if !self.slow {
            self.set_notice(None);
        }
        let previews: Vec<_> = prompt
            .images
            .iter()
            .filter_map(|image| {
                let mut preview = image.preview.clone()?;
                preview.original = self.originals.insert(image.data.clone());
                Some(preview)
            })
            .collect();
        self.make_preview_room(&previews);
        self.push_turn(Turn {
            previews,
            prompt: shown,
            sent: now(),
            items: Vec::new(),
            end: None,
            files: None,
            from,
            switch: None,
        });
        match self.link {
            Link::Open => {
                self.set_state(ChatState::Answering);
                actions.push(self.prompt(prompt));
            }
            Link::Opening => self.queued = Some(prompt),
            Link::Closing => {
                self.queued = Some(prompt);
                self.set_state(ChatState::Starting);
            }
            Link::Closed => {
                self.queued = Some(prompt);
                self.link = Link::Opening;
                self.set_state(ChatState::Starting);
                actions.push(Action::Connect);
            }
        }
    }

    // --- Other agents -------------------------------------------------------

    /// Where the chat stands among the chats agents talk in.
    pub(crate) fn relations(&self) -> &Relations {
        &self.relations
    }

    /// The chat was started by the agent of the chat `parent`, which goes
    /// back to the user's chat `root`, `depth` deep. It starts in `mode`,
    /// the mode its parent was in, or with none, in its agent's own: never
    /// in clackatoo's choice for a new chat, which may let it do more than
    /// its parent can.
    pub(crate) fn started_by(
        &mut self,
        parent: TerminalId,
        root: TerminalId,
        depth: u8,
        mode: Option<String>,
    ) {
        self.relations.parent = Some(parent);
        self.relations.root = Some(root);
        self.relations.depth = depth;
        self.starting_mode = mode;
    }

    /// The chat that started it has closed, and `parent`, which started
    /// that one, takes it on: its card is there now, and its tools reach
    /// from there.
    pub(crate) fn adopted_by(&mut self, parent: TerminalId) {
        self.relations.parent = Some(parent);
    }

    /// The mode the agent is in, by its value, if it offers modes.
    pub(crate) fn mode(&self) -> Option<&str> {
        (self.node.settings.iter())
            .find(|setting| setting.kind == SettingKind::Mode)
            .map(|setting| setting.value.as_str())
    }

    /// Takes `message`, from another chat's agent: it goes at once, as a
    /// turn of its own, unless the agent is answering. Then it waits for
    /// the turn to end, behind any others waiting; with
    /// [`MAX_INBOX`] waiting already, it's handed back. Returns what the
    /// server must do to send it, if it went.
    pub(crate) fn deliver(&mut self, message: Message) -> Result<Vec<Action>, Message> {
        if self.answering() || !self.relations.inbox.is_empty() {
            if self.relations.inbox.len() >= MAX_INBOX {
                return Err(message);
            }
            self.relations.inbox.push_back(message);
            return Ok(Vec::new());
        }
        Ok(self.begin_message(message))
    }

    /// Sends the first message waiting from another agent, if the agent
    /// has finished answering. Returns what the server must do to send it.
    pub(crate) fn deliver_next(&mut self) -> Vec<Action> {
        if self.answering() || self.user_queue.native.is_some() || self.user_queue.restart.is_some()
        {
            return Vec::new();
        }
        match self.relations.inbox.pop_front() {
            Some(message) => self.begin_message(message),
            None => Vec::new(),
        }
    }

    /// Starts a turn with `message`, from another agent.
    fn begin_message(&mut self, message: Message) -> Vec<Action> {
        let mut actions = Vec::new();
        self.relations.hops = message.hops;
        self.relations.cause = message.cause;
        let prompt = relations::prompt_for(&message);
        self.begin(
            message.text,
            prompt.into(),
            Some(message.from),
            &mut actions,
        );
        actions
    }

    /// The chat that started it has had the answers to its first `turns`
    /// turns.
    pub(crate) fn told_parent(&self, turns: usize) {
        self.told_parent.set(self.told_parent.get().max(turns));
    }

    /// Whether the chat that started it has had the answer to every turn
    /// it has finished.
    pub(crate) fn parent_has_heard(&self) -> bool {
        self.told_parent.get() >= self.node.turns.len()
    }

    /// Whether a message has been sent and not yet answered.
    pub(crate) fn is_answering(&self) -> bool {
        self.answering()
    }

    /// The conversation as it is now.
    pub(crate) fn node(&self) -> &ChatNode {
        &self.node
    }

    /// The chat as other chats' agents know it.
    pub(crate) fn sender(&self) -> Sender {
        Sender {
            chat: self.id,
            agent: self.node.agent.clone(),
            title: self.title.clone(),
        }
    }

    /// Stops the agent answering, or takes back the message if it hasn't
    /// gone yet, and stops waiting for a session asked for, even with no
    /// message: one that comes after all is closed unseen, and the next
    /// message asks again.
    fn stop(&mut self, actions: &mut Vec<Action>) {
        if let Some((id, _, _)) = self.user_queue.native {
            self.steered(id, Err("Stopped before acknowledgment".into()));
        }
        self.user_queue.restart = None;
        self.pause_queue(true);
        if !self.answering() && self.link != Link::Opening {
            return;
        }
        // Questions waiting are answered with none, as the protocol asks.
        // An offer to install is turned down, and a suggestion to install,
        // which nothing waits for, stays.
        for (asker, question) in std::mem::take(&mut self.asking) {
            match asker {
                Asker::Suggest => self.asking.push_back((asker, question)),
                Asker::Install => {
                    self.link = Link::Closed;
                    self.set_state(ChatState::Ready);
                }
                Asker::Slow => {}
                asker => actions.extend(self.answer(asker, None)),
            }
        }
        self.show_question();
        if self.link == Link::Open {
            actions.push(Action::Cancel);
        } else if self.queued.take().is_some() {
            self.end_turn(Outcome::Stopped);
        }
        if self.link == Link::Opening {
            self.stop_waiting_for_load(Held::End(Outcome::Stopped));
            self.link = Link::Closed;
            self.set_state(ChatState::Ready);
            self.clear_slow();
            actions.push(Action::Abandon);
        }
        // The user wants the agent to stop: messages from other agents
        // waiting their turn don't start it again.
        let dropped = std::mem::take(&mut self.relations.inbox).len();
        if dropped > 0 {
            let plural = if dropped == 1 { "" } else { "s" };
            self.set_notice(Some(format!(
                "Stopped: {dropped} message{plural} from other agents waiting their turn \
                 weren't sent."
            )));
        }
    }

    // --- What the agent says ------------------------------------------------

    /// The session asked for was made, or opened again. Returns the
    /// message waiting for it, to send.
    ///
    /// The settings the user chose go first, as the agent forgot them if
    /// it stopped, and a new chat asks for its starting mode.
    pub(crate) fn connected(&mut self, opened: Opened) -> Vec<Action> {
        self.link = Link::Open;
        self.clear_slow();
        // Opening one again answers with no ID: it's the one asked for.
        let fresh = !opened.session.is_empty();
        if fresh {
            self.session = Some(opened.session);
        }
        if let Some(options) = &opened.settings {
            self.set_settings(settings_of(options));
        }
        if let Some(carry) = self.carry.take() {
            self.carry_settings(carry);
        }
        let starting = self.starting_mode.take();
        if let Some(mode) = starting.clone()
            && let Some(setting) = (self.node.settings.iter()).find(|setting| {
                setting.kind == SettingKind::Mode
                    && setting.choices.iter().any(|choice| choice.value == mode)
            })
            && !self.chosen.iter().any(|(id, _)| *id == setting.id)
        {
            self.chosen.push((setting.id.clone(), mode));
        } else if starting.is_none()
            && fresh
            && self.relations.parent.is_some()
            && let Some(setting) = self.setting_of(SettingKind::Mode)
            && !self.chosen.iter().any(|(id, _)| *id == setting.id)
            && let Some(safest) = setting.choices.first()
        {
            // A chat an agent started, with no mode of its parent's to
            // start in, as with another agent: never the agent's own
            // choice, which may let it do more than its parent could, but
            // its safest, the first it offers, as Manual or Read-only.
            let safest = (setting.id.clone(), safest.value.clone());
            self.chosen.push(safest);
        }
        // A chat that may run no commands opens each new session in its
        // agent's read-only mode, where it has one, whether or not what it
        // was started wanting came through an upgrade.
        let mut wanted = self.wanted.take().unwrap_or_default();
        wanted.read_only |= fresh && self.limits().no_commands();
        self.choose_wanted(wanted);
        let mut actions = Vec::new();
        for (setting, value) in self.chosen.clone() {
            if chat_wide(&setting) {
                continue;
            }
            let differs = (self.node.settings.iter())
                .any(|known| known.id == setting && known.value != value);
            if differs && self.set_value(&setting, &value) {
                actions.push(Action::Set { setting, value });
            }
        }
        let queued = self.queued.take();
        self.set_state(if queued.is_some() {
            ChatState::Answering
        } else {
            ChatState::Ready
        });
        actions.extend(queued.map(|text| self.prompt(text)));
        actions
    }

    /// What sends `text`, the message of the last turn, to the agent:
    /// after a digest of the turns before it the session hasn't seen, if
    /// there are any. The session has seen them all once that's answered,
    /// without failing; a message sent with nothing to carry is seen as
    /// it's sent.
    fn prompt(&mut self, text: Prompt) -> Action {
        if let Some(role) = self.role.take() {
            self.known = self.node.turns.len();
            return Action::Briefed { role, text };
        }
        let to = self.node.turns.len().saturating_sub(1);
        if self.known < to {
            self.heard = Some(self.node.turns.len());
            Action::Carry {
                text,
                from: self.known,
                to,
            }
        } else {
            self.known = self.node.turns.len();
            Action::Prompt(text)
        }
    }

    /// A digest of turns `from` up to `to` for the chat's agent, which
    /// hasn't seen them, with the files they changed: `None` if there's
    /// nothing in them to tell. It keeps to the chat's budget, at most
    /// [`handoff::MOST`] and a quarter of the agent's context, if it's
    /// said how big that is.
    pub(crate) fn digest(&self, from: usize, to: usize, files: &[Changed]) -> Option<String> {
        let context = (self.node.usage).map_or(usize::MAX, |usage| {
            usize::try_from(usage.size / 4).unwrap_or(usize::MAX)
        });
        Digest {
            turns: &self.node.turns,
            from,
            to,
            agent: &self.node.agent,
            catching_up: from > 0,
            cwd: &self.cwd,
            files,
            plan: &self.plan,
            plan_by: self.plan_by.as_deref(),
            budget: self.budget.min(handoff::MOST).min(context),
            tag: fence_tag(self.id.0),
        }
        .make()
    }

    /// Moves the chat to the agent `to`, on `model`, or the agent's own
    /// choice, with `settings` the settings it last gave, if known, and
    /// a digest of the conversation of at most `budget` tokens. Returns
    /// the agent and session it moved from, for the server to close, or
    /// `None` if it didn't move: the agent is answering, or it's the same
    /// agent.
    ///
    /// A session the chat had with `to` before is gone back to; else a
    /// new one starts. Either hears of the turns it hasn't seen with the
    /// next message.
    #[allow(
        clippy::too_many_lines,
        reason = "each part of moving an agent session"
    )]
    pub(crate) fn switch(
        &mut self,
        to: &AgentSpec,
        model: Option<String>,
        settings: Option<&[Setting]>,
        budget: usize,
    ) -> Option<(String, Option<String>)> {
        if self.answering() || self.user_queue.native.is_some() {
            self.set_notice(Some(format!(
                "{} is still answering: stop it first, or switch once it's done.",
                self.node.agent
            )));
            return None;
        }
        self.budget = budget;
        if to.name == self.agent {
            return None;
        }
        self.set_native_steering(false);
        // The chat's own settings go with it to every agent: what it may
        // do with clackatoo's tools, whether it keeps Claude Code's Task
        // tool, which a session with Claude Code opens with, and the
        // profile it was started as, with its limits.
        let wide: Vec<(String, String)> = (self.chosen.iter())
            .filter(|(id, _)| chat_wide(id))
            .cloned()
            .collect();
        let own = self.agent_settings();
        let of = |kind| own.iter().find(|setting| setting.kind == kind).cloned();
        let (mode, effort) = (of(SettingKind::Mode), of(SettingKind::Effort));
        let left = Parked {
            agent: std::mem::replace(&mut self.agent, to.name.clone()),
            title: self.node.agent.clone(),
            session: self.session.take(),
            known: self.known,
            chosen: (self.chosen.drain(..))
                .filter(|(id, _)| !chat_wide(id))
                .collect(),
            settings: own,
        };
        let back = (self.parked.iter())
            .position(|parked| parked.agent == to.name)
            .map(|at| self.parked.remove(at));
        self.parked.retain(|parked| parked.agent != left.agent);
        let from = (left.agent.clone(), left.session.clone());
        let from_title = left.title.clone();
        self.parked.push(left);

        // Named after its agent, it's named after the new one.
        if self.title == from_title {
            self.title.clone_from(&to.title);
        }
        to.title.clone_into(&mut self.node.agent);
        self.journal.push(ChatPatch::Agent(to.title.clone()));
        let resumed = back.is_some();
        let shown: Vec<Setting> = if let Some(back) = back {
            self.session = back.session;
            self.known = back.known;
            self.chosen = back.chosen;
            self.starting_mode = None;
            back.settings
        } else {
            self.known = 0;
            // An agent's chat never starts in more than the user chose.
            self.starting_mode =
                (to.starting_mode.clone()).filter(|_| self.relations.parent.is_none());
            settings.map(<[Setting]>::to_vec).unwrap_or_default()
        };
        self.chosen.retain(|(id, _)| !chat_wide(id));
        self.chosen.extend(wide);
        if !resumed {
            self.keep_read_only();
        }
        let model_name = model.as_ref().and_then(|value| {
            let setting = shown
                .iter()
                .find(|setting| setting.kind == SettingKind::Model)?;
            let choice = setting
                .choices
                .iter()
                .find(|choice| choice.value == *value)?;
            Some(choice.name.clone())
        });
        self.set_settings(shown);
        self.forget_usage();
        self.clear_slow();
        self.set_notice(None);
        self.withdraw_install();
        self.push_turn(Turn {
            previews: Vec::new(),
            prompt: String::new(),
            sent: now(),
            items: Vec::new(),
            end: Some(TurnEnd {
                at: now(),
                outcome: Outcome::Done,
            }),
            files: None,
            from: None,
            switch: Some(Switch {
                from: from_title,
                to: to.title.clone(),
                model: model_name,
                resumed,
                notes: Vec::new(),
            }),
        });
        self.carry = Some(Carry {
            turn: self.node.turns.len() - 1,
            model,
            mode: if resumed { None } else { mode },
            effort: if resumed { None } else { effort },
        });
        self.link = Link::Opening;
        self.set_state(ChatState::Starting);
        Some(from)
    }

    /// The agent's own settings, as it last gave them, without the
    /// chat's.
    pub(crate) fn agent_settings(&self) -> Vec<Setting> {
        (self.node.settings.iter())
            .filter(|setting| !shown_own(&setting.id))
            .cloned()
            .collect()
    }

    /// The other agents the chat could move to are `providers`.
    pub(crate) fn set_providers(&mut self, providers: Vec<Provider>) {
        if self.node.providers != providers {
            self.node.providers.clone_from(&providers);
            self.journal.push(ChatPatch::Providers(providers));
        }
    }

    /// The agent wouldn't open the session the chat had with it, as it
    /// was asked to: the chat starts a new one, which hears the whole
    /// conversation with the next message, rather than failing.
    pub(crate) fn session_lost(&mut self) {
        self.session = None;
        self.known = 0;
        self.link = Link::Opening;
        // The line that said it went back to it is wrong now.
        let moved = (self.node.turns.iter()).rposition(|turn| turn.switch.is_some());
        if let Some(at) = moved
            && let Some(switch) = self.node.turns[at].switch.as_mut()
            && switch.resumed
        {
            switch.resumed = false;
            self.patch_turn(at);
        }
    }

    /// Gives the settings the new agent's session opened with the values
    /// the chat had with the agent it moved from, where they mean the
    /// same, and says in the turn that shows the move which don't.
    fn carry_settings(&mut self, carry: Carry) {
        let mut notes = Vec::new();
        let agent = self.node.agent.clone();
        let mut chosen = Vec::new();
        if let Some(model) = &carry.model
            && let Some(setting) = self.setting_of(SettingKind::Model)
            && setting.choices.iter().any(|choice| choice.value == *model)
        {
            chosen.push((setting.id.clone(), model.clone()));
        }
        for old in [carry.mode, carry.effort].into_iter().flatten() {
            let Some(was) = old.current() else { continue };
            let Some(setting) = self.setting_of(old.kind) else {
                notes.push(format!(
                    "{} {} doesn't carry over: {agent} has no {}",
                    old.name,
                    was.name,
                    old.name.to_lowercase()
                ));
                continue;
            };
            let same = (setting.choices.iter())
                .find(|choice| choice.value == was.value)
                .or_else(|| {
                    (setting.choices.iter())
                        .find(|choice| choice.name.eq_ignore_ascii_case(&was.name))
                });
            if let Some(choice) = same {
                chosen.push((setting.id.clone(), choice.value.clone()));
                continue;
            }
            // A chat an agent started gets the agent's safest mode, the
            // first it offers, as Manual or Read-only, never more than its
            // parent chose.
            if setting.kind == SettingKind::Mode
                && self.relations.parent.is_some()
                && let Some(safest) = setting.choices.first()
            {
                chosen.push((setting.id.clone(), safest.value.clone()));
                notes.push(format!(
                    "{} {} doesn't carry over: {agent} uses {}, its safest, as an agent started \
                     this chat",
                    old.name, was.name, safest.name
                ));
                continue;
            }
            // What it'll have: the mode a new chat with it asks for, or
            // else its own.
            let value = (self.starting_mode.as_ref())
                .filter(|_| setting.kind == SettingKind::Mode)
                .filter(|mode| setting.choices.iter().any(|choice| choice.value == **mode))
                .unwrap_or(&setting.value);
            let name = (setting.choices.iter())
                .find(|choice| choice.value == *value)
                .map_or(value.as_str(), |choice| choice.name.as_str());
            notes.push(format!(
                "{} {} doesn't carry over: {agent} uses {name}",
                old.name, was.name
            ));
        }
        for (id, value) in chosen {
            self.chosen.retain(|(known, _)| *known != id);
            self.chosen.push((id, value));
        }
        let model = (self.setting_of(SettingKind::Model)).map(|setting| {
            let value = (self.chosen.iter())
                .find(|(id, _)| *id == setting.id)
                .map_or(&setting.value, |(_, value)| value);
            (setting.choices.iter())
                .find(|choice| choice.value == *value)
                .map_or_else(|| value.clone(), |choice| choice.name.clone())
        });
        let turn = self.node.turns.get_mut(carry.turn);
        if let Some(switch) = turn.and_then(|turn| turn.switch.as_mut()) {
            switch.notes = notes;
            if model.is_some() {
                switch.model = model;
            }
            self.patch_turn(carry.turn);
        }
    }

    /// The agent's setting of `kind`, if it has one.
    fn setting_of(&self, kind: SettingKind) -> Option<&Setting> {
        self.node
            .settings
            .iter()
            .find(|setting| setting.kind == kind)
    }

    /// The agent's settings after the chat asked to change one, or why it
    /// couldn't.
    pub(crate) fn settings_changed(&mut self, result: Result<Vec<SessionConfigOption>, String>) {
        match result {
            Ok(options) if !options.is_empty() => self.set_settings(settings_of(&options)),
            Ok(_) => {}
            Err(err) => self.set_notice(Some(format!("The setting couldn't be changed: {err}"))),
        }
    }

    /// The chat's agent isn't installed: asks, in `what`, whether to
    /// install it, and holds any message until it's answered.
    pub(crate) fn offer_install(&mut self, what: &str) {
        self.ask_to_install(Asker::Install, what);
    }

    /// Asks `what`, whether to install the chat's agent, for `asker`.
    fn ask_to_install(&mut self, asker: Asker, what: &str) {
        let question = Question {
            id: self.next_question,
            title: clean(what),
            choices: vec![
                Choice {
                    id: INSTALL.to_owned(),
                    name: "Install".to_owned(),
                    kind: ChoiceKind::AllowOnce,
                },
                Choice {
                    id: "later".to_owned(),
                    name: "Not now".to_owned(),
                    kind: ChoiceKind::RejectOnce,
                },
            ],
            own: true,
        };
        self.next_question += 1;
        self.asking.push_back((asker, question));
        self.show_question();
    }

    /// Whether the chat waits for the session it asked for.
    pub(crate) fn is_opening(&self) -> bool {
        self.link == Link::Opening
    }

    /// The agent hasn't opened the session the chat asked for in a while:
    /// says so in the notice, which `why` is, and asks whether to retry,
    /// starting the agent again, keep waiting, or stop. It's no failure:
    /// the session may still come, and the question goes when it does.
    pub(crate) fn slow_to_open(&mut self, why: String) {
        if self.link != Link::Opening {
            return;
        }
        self.set_notice(Some(why));
        self.slow = true;
        if (self.asking.iter()).any(|(asker, _)| *asker == Asker::Slow) {
            return;
        }
        let question = Question {
            id: self.next_question,
            title: format!(
                "Keep waiting for {}, start it again and ask again, or stop?",
                self.node.agent
            ),
            choices: (SLOW_CHOICES.iter())
                .map(|&(id, name, kind)| Choice {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    kind,
                })
                .collect(),
            own: true,
        };
        self.next_question += 1;
        // First: it's why the chat is stuck, and a suggestion to install
        // the agent, which nothing waits for, mustn't hide it.
        self.asking.push_front((Asker::Slow, question));
        self.show_question();
    }

    /// The user answered `option` to the question about a session the
    /// agent is slow to open.
    fn slow_answered(&mut self, option: Option<&str>, actions: &mut Vec<Action>) {
        match option {
            Some("retry") if self.link == Link::Opening => {
                self.clear_slow();
                self.set_notice(Some(format!("Starting {} again…", self.node.agent)));
                self.slow = true;
                actions.push(Action::Retry);
            }
            Some("stop") => self.stop(actions),
            // Keep waiting, or no answer, or one newer than this build
            // knows: nothing that gives up, or starts anything again.
            _ => {}
        }
    }

    /// The agent is asked again for the session, without being started
    /// again, as another chat is in the middle of a turn with it.
    pub(crate) fn asked_again(&mut self) {
        // Asked to resume it now: it has its turns, as far as they came.
        self.stop_waiting_for_load(Held::Send);
        self.set_notice(Some(format!(
            "Asking {} again. It isn't started again, as another chat is in the middle of a \
             turn with it.",
            self.node.agent
        )));
        self.slow = true;
    }

    /// Takes away what says the agent is slow to open the session: it
    /// came, or isn't waited for.
    fn clear_slow(&mut self) {
        (self.asking).retain(|(asker, _)| *asker != Asker::Slow);
        self.show_question();
        if std::mem::take(&mut self.slow) {
            self.set_notice(None);
        }
    }

    /// Takes back an offer to install the chat's agent, if one is asked.
    fn withdraw_install(&mut self) {
        (self.asking).retain(|(asker, _)| !matches!(asker, Asker::Install | Asker::Suggest));
        self.show_question();
    }

    /// Whether the chat is asking whether to install its agent.
    pub(crate) fn offering_install(&self) -> bool {
        (self.asking.iter()).any(|(asker, _)| matches!(asker, Asker::Install | Asker::Suggest))
    }

    /// Whether the chat's session is open.
    pub(crate) fn is_connected(&self) -> bool {
        self.link == Link::Open
    }

    /// Lets its session go while it has nothing to do, if it's open and
    /// nothing waits on it, returning it for the server to close. The
    /// chat keeps its ID: its next message opens it again, as after an
    /// upgrade.
    pub(crate) fn park(&mut self) -> Option<String> {
        let idle = !self.answering() && self.asking.is_empty() && self.relations.inbox.is_empty();
        if self.link != Link::Open || !idle {
            return None;
        }
        self.link = Link::Closing;
        self.session.clone()
    }

    /// The agent wasn't asked to end the session [`park`](Self::park)
    /// let go after all, as it can't: nothing to wait for.
    pub(crate) fn parked_without_closing(&mut self) {
        if self.link == Link::Closing {
            self.link = Link::Closed;
        }
    }

    /// Its agent is being started again. Returns whether the chat must ask
    /// the new process for its session: it was waiting for one, or let it
    /// go while idle with a message coming meanwhile, which waits for it
    /// still. Any other session it had is gone, and its next message opens
    /// it again, as after the agent stopped.
    pub(crate) fn agent_restarted(&mut self) -> bool {
        match self.link {
            Link::Opening => {
                self.load_cut_off(Held::Send);
                true
            }
            // The agent stopping ends the session it was letting go.
            Link::Closing => !self.session_closed().is_empty(),
            Link::Open | Link::Closed => {
                self.disconnected(None);
                false
            }
        }
    }

    /// The session [`park`](Self::park) let go has ended. Returns what
    /// opens it again, if a message came meanwhile.
    pub(crate) fn session_closed(&mut self) -> Vec<Action> {
        if self.link != Link::Closing {
            return Vec::new();
        }
        if self.queued.is_some() {
            self.link = Link::Opening;
            vec![Action::Connect]
        } else {
            self.link = Link::Closed;
            Vec::new()
        }
    }

    /// The chat's agent runs, but not from where clackatoo installs it:
    /// suggests, in `what`, installing it there, waiting for nothing.
    pub(crate) fn suggest_install(&mut self, what: &str) {
        if !self.asking.is_empty() {
            return;
        }
        self.ask_to_install(Asker::Suggest, what);
    }

    /// The chat's agent, which runs another way meanwhile, has been
    /// installed, or couldn't be: why.
    pub(crate) fn installed_meanwhile(&mut self, outcome: &Result<(), String>) {
        self.withdraw_install();
        self.set_notice(outcome.as_ref().err().cloned());
    }

    /// The chat's agent is being installed, as another chat asked: it
    /// waits for that.
    pub(crate) fn installing(&mut self) {
        self.set_state(ChatState::Starting);
        self.set_notice(Some(format!("Installing {}…", self.node.agent)));
    }

    /// The chat's agent has been installed: the server gets it a session
    /// next.
    pub(crate) fn installed(&mut self) {
        self.withdraw_install();
        self.link = Link::Opening;
        self.set_notice(None);
    }

    /// The session couldn't be made or opened, nor the agent installed.
    pub(crate) fn failed_to_connect(&mut self, reason: &str) {
        self.pause_queue(true);
        self.clear_slow();
        self.set_notice(None);
        self.withdraw_install();
        self.link = Link::Closed;
        if self.queued.take().is_some() {
            self.end_turn(Outcome::Failed(format!(
                "{} couldn't start: {reason}",
                self.node.agent
            )));
        }
        // A load that couldn't be made is made with the next message.
        self.release_held(Held::End(Outcome::Failed(format!(
            "{} couldn't start: {reason}",
            self.node.agent
        ))));
        self.set_state(ChatState::Failed(reason.to_owned()));
    }

    /// The chat's agent is going, so it can't be asked for the chat's
    /// session: the chat waits for it to have gone, to ask a new one, and
    /// any message it holds waits with it.
    pub(crate) fn wait_for_agent_to_go(&mut self) {
        self.clear_slow();
        self.link = Link::Opening;
        self.set_state(ChatState::Starting);
        self.set_notice(Some(format!(
            "{} is stopping. It starts again once it has.",
            self.node.agent
        )));
    }

    /// The agent the chat waited for to go has gone: returns whether the
    /// chat is still waiting to ask a new one for its session, as it
    /// hasn't been stopped meanwhile.
    pub(crate) fn agent_went(&mut self) -> bool {
        if self.link != Link::Opening {
            return false;
        }
        self.set_notice(None);
        true
    }

    /// The agent has gone: it exited, or stopped answering. If it never
    /// started, `why` says why, as far as it said.
    pub(crate) fn disconnected(&mut self, why: Option<&str>) {
        self.queue_disconnected();
        self.clear_slow();
        self.load_cut_off(Held::End(Outcome::Failed("the agent stopped".to_owned())));
        let was_opening = self.link == Link::Opening;
        self.link = Link::Closed;
        let queued = self.queued.take();
        if let Some(why) = why.filter(|_| was_opening) {
            let why = format!("it stopped, saying: {why}");
            if queued.is_some() {
                self.end_turn(Outcome::Failed(format!(
                    "{} couldn't start: {why}",
                    self.node.agent
                )));
            }
            self.set_state(ChatState::Failed(why));
        } else if self.answering() {
            self.end_turn(Outcome::Failed("the agent stopped".to_owned()));
            self.set_state(ChatState::Ready);
        } else if was_opening {
            self.set_state(ChatState::Failed("it stopped before it started".to_owned()));
        } else if self.session.is_some() {
            self.set_notice(Some(
                "The agent stopped. The next message starts it again.".to_owned(),
            ));
        }
    }

    /// The agent finished answering, or failed to.
    pub(crate) fn answered(&mut self, result: Result<StopReason, String>) {
        let outcome = match result {
            Ok(StopReason::Cancelled) => Outcome::Stopped,
            Ok(StopReason::MaxTokens) => {
                Outcome::Failed("the answer hit its length limit".to_owned())
            }
            Ok(StopReason::MaxTurnRequests) => {
                Outcome::Failed("the answer took too many steps".to_owned())
            }
            Ok(StopReason::Refusal) => Outcome::Failed("the agent refused".to_owned()),
            // Done, or a reason newer than this build knows.
            Ok(_) => Outcome::Done,
            Err(err) => Outcome::Failed(format!("the agent failed: {err}")),
        };
        if matches!(outcome, Outcome::Failed(_)) {
            self.user_queue.restart = None;
        }
        if !matches!(outcome, Outcome::Done) {
            self.pause_queue(true);
        }
        if self.answering() {
            self.end_turn(outcome);
        }
        self.set_state(ChatState::Ready);
    }

    /// The option that lets `tool` go ahead once, if the agent asks
    /// whether it may use one of clackatoo's own tools: those ask the
    /// user themselves when they must, so the agent's question is answered
    /// at once, and the user isn't asked twice. Codex asks about every
    /// MCP tool call, as a call titled `mcp.clackatoo.TOOL`, with the
    /// server in its input; Claude Code is told not to ask.
    /// Notes the tool call `id` as one of clackatoo's own if its `input`
    /// names clackatoo's MCP server.
    fn note_own_tool(&mut self, id: &str, input: Option<&Value>) {
        /// How many are kept: more than run at once.
        const KEPT: usize = 64;
        // A tool told again was called long ago: no question now is
        // about it.
        if self.told_again() {
            return;
        }
        if names_our_server(input) && !self.own_tools.iter().any(|own| own == id) {
            if self.own_tools.len() == KEPT {
                self.own_tools.pop_front();
            }
            self.own_tools.push_back(id.to_owned());
        }
    }

    pub(crate) fn about_own_tools(
        &self,
        tool: &ToolCallUpdate,
        options: &[PermissionOption],
    ) -> Option<String> {
        let id = tool.tool_call_id.0.to_string();
        let title = (self.tool(&id).map(|tool| tool.title.clone()))
            .or_else(|| tool.fields.title.clone())
            .unwrap_or_default();
        let name = crate::mcp::SERVER_NAME;
        // A title is the agent's to word: Codex titles a command with the
        // command itself. Only a tool that edits or runs nothing is known
        // as ours by its title alone; one naming its server is ours.
        let kind = (self.tool(&id).map(|tool| tool.kind))
            .or_else(|| tool.fields.kind.map(tool_kind))
            .unwrap_or_default();
        let harmless = !matches!(
            kind,
            ToolKind::Edit | ToolKind::Delete | ToolKind::Move | ToolKind::Execute
        );
        let ours = names_our_server(tool.fields.raw_input.as_ref())
            || self.own_tools.iter().any(|own| *own == id)
            || harmless
                && (title.starts_with(&format!("mcp.{name}."))
                    || title.starts_with(&format!("mcp__{name}__")));
        if !ours || self.tools() == AgentTools::Off {
            return None;
        }
        (options.iter())
            .find(|option| option.kind == PermissionOptionKind::AllowOnce)
            .map(|option| option.option_id.0.to_string())
    }

    /// The agent asks whether it may use a tool.
    pub(crate) fn ask(
        &mut self,
        request: Value,
        tool: &ToolCallUpdate,
        options: &[PermissionOption],
    ) {
        // The tool's own title, if it's been seen: Codex's questions call
        // every command "Run command", but its tool call has the command.
        let id = tool.tool_call_id.0.to_string();
        let title = (self.tool(&id).map(|tool| tool.title.clone()))
            .or_else(|| tool.fields.title.clone())
            .unwrap_or_else(|| "use a tool".to_owned());
        let choices = options
            .iter()
            .map(|option| Choice {
                id: option.option_id.0.to_string(),
                name: option.name.clone(),
                kind: match option.kind {
                    PermissionOptionKind::AllowAlways => ChoiceKind::AllowAlways,
                    PermissionOptionKind::RejectOnce => ChoiceKind::RejectOnce,
                    PermissionOptionKind::RejectAlways => ChoiceKind::RejectAlways,
                    // Allowing once, or a kind newer than this build knows.
                    _ => ChoiceKind::AllowOnce,
                },
            })
            .collect();
        let question = Question {
            id: self.next_question,
            title: clean(&title),
            choices,
            own: false,
        };
        self.next_question += 1;
        self.asking.push_back((Asker::Agent(request), question));
        self.show_question();
    }

    /// One of clackatoo's tools asks whether the agent may do `what`, as
    /// "run `make` in a terminal", for the call the server numbered
    /// `call`. The answer comes back as [`Action::Permit`]. `whole` is
    /// what the agent would type, if it's too long for the question: it
    /// goes in the conversation, in full, for the user to read first.
    /// Returns whether it was asked: too many questions waiting refuse it.
    pub(crate) fn ask_for_tool(
        &mut self,
        call: u64,
        what: &str,
        whole: Option<&str>,
        offered: Choices,
    ) -> bool {
        if self.asking.len() >= MAX_QUESTIONS {
            return false;
        }
        if let Some(whole) = whole {
            self.set_tool(Tool {
                id: format!("clackatoo-{call}"),
                title: "What the agent asks to type, in full".to_owned(),
                status: ToolStatus::Done,
                kind: ToolKind::Other,
                content: vec![ToolContent::Text(format!("```\n{whole}\n```"))],
                locations: Vec::new(),
            });
        }
        let choices: &[(&str, &str, ChoiceKind)] = match offered {
            Choices::Tool => &TOOL_CHOICES,
            Choices::Talk => &TALK_CHOICES,
            Choices::Browser => &BROWSER_CHOICES,
        };
        let choices = (choices.iter())
            .map(|&(id, name, kind)| Choice {
                id: id.to_owned(),
                name: name.to_owned(),
                kind,
            })
            .collect();
        // Shown as given: its control characters are already made
        // visible, and nothing of it may go unseen.
        // About what the agent would do: never the daemon's own.
        let question = Question {
            id: self.next_question,
            title: what.to_owned(),
            choices,
            own: false,
        };
        self.next_question += 1;
        self.asking.push_back((Asker::Tool(call), question));
        self.show_question();
        true
    }

    /// Takes back the question about the tool call `call`, which no longer
    /// waits for an answer: the agent cancelled it, or went.
    pub(crate) fn withdraw(&mut self, call: u64) {
        self.asking.retain(|(asker, _)| *asker != Asker::Tool(call));
        self.show_question();
    }

    /// What to do about the answer `option` to `asker`'s question.
    /// "Always allow" for clackatoo's tools lets the agent run commands in
    /// its own terminals without asking from then on, until the user
    /// changes the chat's Tools setting back. "Always allow for that
    /// chat" is the server's to remember, and changes no setting.
    fn answer(&mut self, asker: Asker, option: Option<String>) -> Option<Action> {
        let install = option.as_deref() == Some(INSTALL);
        match asker {
            Asker::Agent(request) => Some(Action::Answer { request, option }),
            Asker::Tool(call) => {
                let option = option.as_deref();
                if option == Some("always") {
                    self.set_tools(AgentTools::Allow);
                }
                if option == Some("browse") {
                    self.chosen.retain(|(id, _)| id != BROWSING_SETTING);
                    (self.chosen).push((BROWSING_SETTING.to_owned(), "yes".to_owned()));
                }
                Some(Action::Permit {
                    call,
                    allow: matches!(option, Some("allow" | "always" | "pair" | "browse")),
                    pair: option == Some("pair"),
                })
            }
            Asker::Install if install => {
                self.installing();
                Some(Action::Install)
            }
            Asker::Install => {
                self.failed_to_connect(
                    "it isn't installed. The next message offers to install it again.",
                );
                None
            }
            Asker::Suggest if install => {
                self.set_notice(Some(format!(
                    "Installing {}'s adapter. The chat carries on meanwhile.",
                    self.node.agent
                )));
                Some(Action::Install)
            }
            // Nothing waits for a suggestion to install. A session slow
            // to open is answered by `slow_answered`, or the question
            // taken away.
            Asker::Suggest | Asker::Slow => None,
        }
    }

    /// Shows the first question waiting, if any.
    fn show_question(&mut self) {
        let question = self.asking.front().map(|(_, question)| question.clone());
        self.set_question(question);
    }

    /// Takes a change to the session. Returns the files a tool that has
    /// finished says it changed, if the change says so: they're on disk
    /// already, as Claude Code's tools write them.
    pub(crate) fn update(&mut self, update: SessionUpdate) -> Option<Wrote> {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => self.write(&chunk.content, false),
            SessionUpdate::AgentThoughtChunk(chunk) => self.write(&chunk.content, true),
            SessionUpdate::ToolCall(call) => {
                self.revoke_image(&call.tool_call_id.0);
                self.note_diffs(&call.tool_call_id.0, &call.content);
                self.note_own_tool(&call.tool_call_id.0, call.raw_input.as_ref());
                let tool = Tool {
                    id: call.tool_call_id.0.to_string(),
                    title: clean(&call.title),
                    status: tool_status(call.status),
                    kind: tool_kind(call.kind),
                    content: tool_content(&call.content),
                    locations: tool_locations(&call.locations),
                };
                let (id, wrote) = (tool.id.clone(), self.wrote(&tool));
                self.set_tool(tool);
                self.add_output(&id, call.meta.as_ref());
                return wrote;
            }
            SessionUpdate::ToolCallUpdate(update) => {
                if let Some(content) = &update.fields.content {
                    self.revoke_image(&update.tool_call_id.0);
                    self.note_diffs(&update.tool_call_id.0, content);
                }
                self.note_own_tool(&update.tool_call_id.0, update.fields.raw_input.as_ref());
                let id = update.tool_call_id.0.to_string();
                let known = self.tool(&id).cloned();
                let mut tool = known.clone().unwrap_or(Tool {
                    id: id.clone(),
                    title: "a tool".to_owned(),
                    status: ToolStatus::Running,
                    kind: ToolKind::Other,
                    content: Vec::new(),
                    locations: Vec::new(),
                });
                if let Some(title) = &update.fields.title {
                    tool.title = clean(title);
                }
                if let Some(status) = update.fields.status {
                    tool.status = tool_status(status);
                }
                if let Some(kind) = update.fields.kind {
                    tool.kind = tool_kind(kind);
                }
                // What it shows is replaced, not added to: a command's
                // description gives way to its output. Output streamed
                // beside stays.
                if let Some(content) = &update.fields.content {
                    let streamed = (tool.content.drain(..))
                        .filter(|content| matches!(content, ToolContent::Output(_)));
                    let streamed: Vec<ToolContent> = streamed.collect();
                    tool.content = tool_content(content);
                    tool.content.extend(streamed);
                }
                if let Some(locations) = &update.fields.locations {
                    tool.locations = tool_locations(locations);
                }
                // Claude Code's adapter says what a tool wrote once it has
                // written it, in an update of its own that may come before
                // or after the one saying it's finished.
                let fields = &update.fields;
                let says = fields.status.is_some()
                    || fields.content.is_some()
                    || fields.locations.is_some();
                let wrote = says.then(|| self.wrote(&tool)).flatten();
                // A piece of output alone changes nothing else, and goes
                // as an append, not the tool again.
                if known.as_ref() != Some(&tool) {
                    self.set_tool(tool);
                }
                self.add_output(&id, update.meta.as_ref());
                return wrote;
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                // Not a change to the turns: viewers pick it up anyway.
                // Codex's is the whole of the first message.
                if let MaybeUndefined::Value(title) = info.title {
                    self.title = clean(&title);
                    if let Some((cut, _)) = self.title.char_indices().nth(TITLE) {
                        self.title.truncate(cut);
                        self.title.push('…');
                    }
                }
            }
            SessionUpdate::ConfigOptionUpdate(update) => {
                self.set_settings(settings_of(&update.config_options));
            }
            SessionUpdate::CurrentModeUpdate(update) => {
                let mode = update.current_mode_id.0.to_string();
                let id = (self.node.settings.iter())
                    .find(|setting| setting.kind == SettingKind::Mode)
                    .map(|setting| setting.id.clone());
                if let Some(id) = id {
                    self.set_value(&id, &mode);
                }
            }
            // What it says of its account's limits the daemon has taken
            // in already, for every chat of the agent: see
            // `Daemon::limits_heard`.
            SessionUpdate::UsageUpdate(usage) => self.set_usage(Some(Usage {
                used: usage.used.min(limits::MOST_TOKENS),
                size: usage.size.min(limits::MOST_TOKENS),
                five_hour: self.limits.five_hour,
                weekly: self.limits.weekly,
            })),
            // Not shown yet, but kept for the next agent to hear of.
            SessionUpdate::Plan(plan) => {
                self.plan = steps(plan);
                self.plan_by = Some(self.node.agent.clone());
            }
            // Its commands, which the chat doesn't show yet, and the
            // user's own words, already kept.
            _ => {}
        }
        None
    }

    /// Notes the changes to files in `content`, which tool `id` shows,
    /// if it shows any, in place of what it showed before.
    fn note_diffs(&mut self, id: &str, content: &[ToolCallContent]) {
        let diffs: Vec<RawDiff> = (content.iter())
            .filter_map(|content| match content {
                ToolCallContent::Diff(diff) => Some(RawDiff {
                    path: diff.path.to_string_lossy().into_owned(),
                    old: diff.old_text.clone(),
                    new: diff.new_text.clone(),
                }),
                _ => None,
            })
            .collect();
        if diffs.is_empty() {
            return;
        }
        self.diffs.retain(|(tool, _)| tool != id);
        self.diffs.push((id.to_owned(), diffs));
    }

    /// The files `tool` changed, if it's a finished edit that names any,
    /// or the files it's about to change, if it's one still running.
    fn wrote(&self, tool: &Tool) -> Option<Wrote> {
        let diffs = (tool.content.iter()).filter_map(|content| match content {
            ToolContent::Diff(diff) => Some(&diff.path),
            ToolContent::Text(_)
            | ToolContent::Output(_)
            | ToolContent::Other(_)
            | ToolContent::Image(_) => None,
        });
        let edits = matches!(tool.kind, ToolKind::Edit | ToolKind::Delete)
            || diffs.clone().next().is_some();
        if tool.status == ToolStatus::Failed || !edits {
            return None;
        }
        let mut paths: Vec<String> = Vec::new();
        let named = diffs.chain(tool.locations.iter().map(|location| &location.path));
        for path in named {
            if !path.is_empty() && !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        // A tool saying what it wrote after its turn ended still wrote it,
        // in the last turn.
        let turn = self.node.turns.len().checked_sub(1)?;
        let items = &self.node.turns[turn].items;
        let order = (items.iter())
            .position(|item| matches!(item, Item::Tool(old) if old.id == tool.id))
            .unwrap_or(items.len());
        let diffs = (self.diffs.iter())
            .find(|(id, _)| *id == tool.id)
            .map(|(_, diffs)| diffs.clone())
            .unwrap_or_default();
        Some(Wrote {
            done: tool.status == ToolStatus::Done,
            turn,
            tool: tool.id.clone(),
            order,
            paths,
            diffs,
        })
        .filter(|wrote| !wrote.paths.is_empty())
    }

    /// Whether `update`, not yet taken, says a tool that may change files
    /// has finished (see [`writes_files`]): whatever it changed on disk is
    /// there by then.
    pub(crate) fn finishes_writing(&self, update: &SessionUpdate) -> bool {
        let finished = |status: ToolCallStatus| {
            matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed)
        };
        match update {
            SessionUpdate::ToolCall(call) => {
                finished(call.status) && writes_files(tool_kind(call.kind))
            }
            SessionUpdate::ToolCallUpdate(update) => {
                let kind = (update.fields.kind.map(tool_kind))
                    .or_else(|| self.tool(&update.tool_call_id.0).map(|tool| tool.kind));
                update.fields.status.is_some_and(finished) && kind.is_none_or(writes_files)
            }
            _ => false,
        }
    }

    /// Whether a tool of the agent's that may change files is running now,
    /// as an edit or `cargo fmt` does, while nothing waits on a question
    /// the agent, or one of clackatoo's tools, has asked. A tool waiting to
    /// be allowed isn't running, and while one is asked about, none is
    /// taken to be.
    pub(crate) fn writing(&self) -> bool {
        // Tools told again as the session loads ran long ago.
        if self.told_again() {
            return false;
        }
        let asked = (self.asking.iter())
            .any(|(asker, _)| matches!(asker, Asker::Agent(_) | Asker::Tool(_)));
        let Some(turn) = self.answering_turn().filter(|_| !asked) else {
            return false;
        };
        self.node.turns[turn].items.iter().any(|item| {
            matches!(item, Item::Tool(tool)
                if tool.status == ToolStatus::Running && writes_files(tool.kind))
        })
    }

    /// The agent, as it's shown: "Claude Code", "Codex"…
    pub(crate) fn agent_title(&self) -> &str {
        &self.node.agent
    }

    /// Says `notice` in the chat, until the next message.
    pub(crate) fn notify(&mut self, notice: String) {
        self.set_notice(Some(notice));
    }

    /// How many turns the chat has.
    pub(crate) fn turns(&self) -> usize {
        self.node.turns.len()
    }

    /// Says that turn `turn` changed `files`.
    pub(crate) fn set_files(&mut self, turn: usize, files: Option<TurnFiles>) {
        if let Some(edited) = self.node.turns.get_mut(turn)
            && edited.files != files
        {
            edited.files = files;
            self.journal.push(ChatPatch::Files {
                turn: index_u32(turn),
                files,
            });
        }
    }

    /// Where the agent is in its work, for what it writes now: the turn
    /// it's answering, or else the last, and the tool it's running, if one
    /// is.
    pub(crate) fn working_on(&self) -> (usize, Option<String>) {
        let turn = self.node.turns.len().saturating_sub(1);
        let running = (self
            .answering_turn()
            .map(|turn| &self.node.turns[turn].items))
        .and_then(|items| {
            items.iter().rev().find_map(|item| match item {
                Item::Tool(tool) if tool.status == ToolStatus::Running => Some(tool.id.clone()),
                _ => None,
            })
        });
        (turn, running)
    }

    /// A piece of the agent's words, or of its thinking, on the end of the
    /// turn answering.
    fn write(&mut self, content: &ContentBlock, thinking: bool) {
        let ContentBlock::Text(text) = content else {
            return;
        };
        let Some(index) = self.answering_turn() else {
            return;
        };
        let text = clean_text(&text.text);
        let items = &mut self.node.turns[index].items;
        let continues = match items.last() {
            Some(Item::Words(_)) => !thinking,
            Some(Item::Thought(_)) => thinking,
            _ => false,
        };
        if !continues {
            // A new item goes whole, empty, with the text appended after.
            let item = if thinking {
                Item::Thought(String::new())
            } else {
                Item::Words(String::new())
            };
            items.push(item.clone());
            let at = items.len() - 1;
            self.patch_item(index, at, item);
        }
        let items = &mut self.node.turns[index].items;
        if let Some(Item::Words(words) | Item::Thought(words)) = items.last_mut() {
            words.push_str(&text);
        }
        let item = items.len() - 1;
        self.journal.push(ChatPatch::Append {
            turn: index_u32(index),
            item: index_u32(item),
            text,
        });
    }

    /// Adds to the output of tool `id`'s command what `meta` streams, as
    /// Codex's adapter streams it (and Zed's convention, `terminal_output`),
    /// rather than in the tool's content. Each piece goes to viewers as an
    /// append. Past twice what a tool keeps, it's cut back to the end of
    /// it, which goes as the tool again.
    fn add_output(&mut self, id: &str, meta: Option<&serde_json::Map<String, Value>>) {
        let Some(meta) = meta else { return };
        let data = ["terminal_output_delta", "terminal_output"]
            .iter()
            .find_map(|key| meta.get(*key)?.get("data")?.as_str());
        let (Some(data), Some(turn)) = (data, self.answering_turn()) else {
            return;
        };
        let items = &mut self.node.turns[turn].items;
        let Some((at, tool)) = (items.iter_mut().enumerate()).find_map(|(at, item)| match item {
            Item::Tool(tool) if tool.id == id => Some((at, tool)),
            _ => None,
        }) else {
            return;
        };
        let text = clean_text(data);
        tool.append_output(&text);
        let output = tool.output_mut().expect("just added to");
        if output.len() > 2 * TOOL_TEXT {
            *output = tail(output, TOOL_TEXT);
            let tool = Item::Tool(tool.clone());
            self.patch_item(turn, at, tool);
        } else {
            self.journal.push(ChatPatch::Output {
                turn: index_u32(turn),
                item: index_u32(at),
                text,
            });
        }
    }

    /// Adds `tool` to the turn answering, or changes the one with its ID.
    fn set_tool(&mut self, tool: Tool) {
        let Some(index) = self.answering_turn() else {
            return;
        };
        let items = &mut self.node.turns[index].items;
        let at =
            (items.iter()).position(|item| matches!(item, Item::Tool(old) if old.id == tool.id));
        let at = if let Some(at) = at {
            items[at] = Item::Tool(tool.clone());
            at
        } else {
            items.push(Item::Tool(tool.clone()));
            items.len() - 1
        };
        // Only the tool goes, not its turn: a turn of many tools, each
        // with its output, would be sent again with every change.
        self.patch_item(index, at, Item::Tool(tool));
    }

    /// The tool with `id`, in the turn answering.
    fn tool(&self, id: &str) -> Option<&Tool> {
        let turn = self.node.turns.get(self.answering_turn()?)?;
        turn.items.iter().find_map(|item| match item {
            Item::Tool(tool) if tool.id == id => Some(tool),
            _ => None,
        })
    }

    // --- Changes ------------------------------------------------------------

    /// Whether a message has been sent and not yet answered: the agent's
    /// turn is running.
    pub(crate) fn answering(&self) -> bool {
        self.answering_turn().is_some()
    }

    /// The turn being answered, if one is.
    fn answering_turn(&self) -> Option<usize> {
        let last = self.node.turns.len().checked_sub(1)?;
        self.node.turns[last].end.is_none().then_some(last)
    }

    /// Ends the turn being answered. Tools still running then never
    /// finished.
    fn end_turn(&mut self, outcome: Outcome) {
        let Some(index) = self.answering_turn() else {
            return;
        };
        // A digest carried with the message is heard, unless it failed.
        if let Some(heard) = self.heard.take()
            && !matches!(outcome, Outcome::Failed(_))
        {
            self.known = heard;
            // Another agent's plan has been told: it's for the user now.
            if self.plan_by.as_ref() != Some(&self.node.agent) {
                self.plan.clear();
                self.plan_by = None;
            }
        }
        let turn = &mut self.node.turns[index];
        for item in &mut turn.items {
            if let Item::Tool(tool) = item
                && tool.status == ToolStatus::Running
            {
                tool.status = ToolStatus::Failed;
            }
        }
        turn.end = Some(TurnEnd { at: now(), outcome });
        self.patch_turn(index);
        // The agent's questions went with the turn. Those of clackatoo's
        // tools wait for their calls to be answered or cancelled, and an
        // offer to install stays until it's answered.
        (self.asking).retain(|(asker, _)| !matches!(asker, Asker::Agent(_)));
        self.show_question();
    }

    pub(crate) fn hand_over_previews(&self) -> handover::ChatPreviews {
        handover::ChatPreviews {
            chat: self.id.0,
            turns: self
                .node
                .turns
                .iter()
                .enumerate()
                .filter(|(_, turn)| !turn.previews.is_empty())
                .map(|(index, turn)| {
                    (
                        index_u32(index),
                        turn.previews
                            .iter()
                            .map(|preview| handover::ChatPreview {
                                width: preview.width,
                                height: preview.height,
                                rgba: preview.rgba.to_vec(),
                            })
                            .collect(),
                    )
                })
                .collect(),
        }
    }

    fn revoke_image(&mut self, id: &str) {
        self.image_receipts
            .retain(|(tool, receipt)| tool != id && receipt.strong_count() > 0);
    }

    fn image_receipt(&mut self, id: &str) -> std::sync::Arc<()> {
        self.revoke_image(id);
        let receipt = std::sync::Arc::new(());
        self.image_receipts
            .push((id.to_owned(), std::sync::Arc::downgrade(&receipt)));
        receipt
    }

    pub(crate) fn image_target(&mut self, id: &str) -> Option<crate::tool_images::Target> {
        let turn = self.node.turns.len().checked_sub(1)?;
        let current = &self.node.turns[turn];
        let tool = current.items.iter().find_map(|item| match item {
            Item::Tool(tool) if tool.id == id => Some(tool),
            _ => None,
        })?;
        Some(crate::tool_images::Target {
            terminal: self.id,
            turn,
            sent: current.sent,
            tool: id.to_owned(),
            content: tool.content.clone(),
            receipt: self.image_receipt(id),
        })
    }

    pub(crate) fn hand_over_tool_previews(&self) -> Vec<handover::ToolPreviews> {
        self.node
            .turns
            .iter()
            .enumerate()
            .flat_map(|(turn, value)| {
                value.items.iter().filter_map(move |item| {
                    let Item::Tool(tool) = item else {
                        return None;
                    };
                    let images: Vec<_> = tool
                        .content
                        .iter()
                        .filter_map(|content| match content {
                            ToolContent::Image(image) => Some(handover::ChatPreview {
                                width: image.width,
                                height: image.height,
                                rgba: image.rgba.to_vec(),
                            }),
                            _ => None,
                        })
                        .collect();
                    (!images.is_empty()).then(|| handover::ToolPreviews {
                        chat: self.id.0,
                        turn: index_u32(turn),
                        tool: tool.id.clone(),
                        images,
                    })
                })
            })
            .collect()
    }

    pub(crate) fn resume_tool_previews(&mut self, records: &mut [handover::ToolPreviews]) {
        let id = self.id.0;
        for record in records.iter_mut().filter(|record| record.chat == id) {
            let Some(turn) = self.node.turns.get(record.turn as usize) else {
                continue;
            };
            let Some(content) = turn.items.iter().find_map(|item| match item {
                Item::Tool(tool) if tool.id == record.tool => Some(tool.content.clone()),
                _ => None,
            }) else {
                continue;
            };
            let target = crate::tool_images::Target {
                terminal: self.id,
                turn: record.turn as usize,
                sent: turn.sent,
                tool: record.tool.clone(),
                content,
                receipt: self.image_receipt(&record.tool),
            };
            let images = std::mem::take(&mut record.images)
                .into_iter()
                .take(4)
                .map(|image| clackatoo_protocol::ChatPreview {
                    original: 0,
                    width: image.width,
                    height: image.height,
                    rgba: image.rgba.into(),
                })
                .collect();
            self.tool_images(&target, images);
        }
    }

    pub(crate) fn tool_originals(
        &mut self,
        target: &crate::tool_images::Target,
        images: Vec<(clackatoo_protocol::ChatPreview, String)>,
    ) -> bool {
        // Validate the receipt and destination before retaining any original data.
        if images.is_empty() || images.len() > 4 || images.iter().any(|(image, _)| !image.valid())
            || !self.node.turns.get(target.turn).is_some_and(|turn| turn.sent == target.sent && turn.items.iter().any(|item| matches!(item, Item::Tool(tool) if tool.id == target.tool && tool.content == target.content))) {
            return false;
        }
        if !self
            .image_receipts
            .iter()
            .any(|(_, receipt)| receipt.ptr_eq(&std::sync::Arc::downgrade(&target.receipt)))
        {
            return false;
        }
        let images = images
            .into_iter()
            .map(|(mut preview, data)| {
                preview.original = self.originals.insert(data);
                preview
            })
            .collect();
        self.tool_images(target, images)
    }

    pub(crate) fn tool_images(
        &mut self,
        target: &crate::tool_images::Target,
        images: Vec<clackatoo_protocol::ChatPreview>,
    ) -> bool {
        if !self
            .image_receipts
            .iter()
            .any(|(_, receipt)| receipt.ptr_eq(&std::sync::Arc::downgrade(&target.receipt)))
            || images.is_empty()
            || images.len() > 4
            || images.iter().any(|image| !image.valid())
        {
            return false;
        }
        let Some(turn) = self
            .node
            .turns
            .get(target.turn)
            .filter(|turn| turn.sent == target.sent)
        else {
            return false;
        };
        let Some(at) = turn.items.iter().position(|item| matches!(item, Item::Tool(tool) if tool.id == target.tool && tool.content == target.content)) else { return false; };
        self.revoke_image(&target.tool);
        self.make_preview_room(&images);
        let Item::Tool(tool) = &mut self.node.turns[target.turn].items[at] else {
            return false;
        };
        tool.content
            .extend(images.into_iter().map(ToolContent::Image));
        let item = Item::Tool(tool.clone());
        self.patch_item(target.turn, at, item);
        true
    }

    pub(crate) fn resume_previews(&mut self, previews: handover::ChatPreviews) {
        let mut bytes = 0;
        for (index, images) in previews.turns.into_iter().rev() {
            let Some(turn) = self.node.turns.get_mut(index as usize) else {
                continue;
            };
            for image in images
                .into_iter()
                .take(clackatoo_protocol::ChatImage::MAX_COUNT)
            {
                let preview = clackatoo_protocol::ChatPreview {
                    original: 0,
                    width: image.width,
                    height: image.height,
                    rgba: image.rgba.into(),
                };
                if preview.valid()
                    && bytes + preview.rgba.len() <= clackatoo_protocol::ChatPreview::HISTORY_BYTES
                {
                    bytes += preview.rgba.len();
                    turn.previews.push(preview);
                }
            }
        }
    }

    /// Retain bounded recent thumbnails; keep older attachment labels.
    fn make_preview_room(&mut self, incoming: &[clackatoo_protocol::ChatPreview]) {
        if incoming.is_empty() {
            return;
        }
        let mut bytes: usize = self
            .node
            .turns
            .iter()
            .flat_map(|turn| {
                turn.previews.iter().chain(
                    turn.items
                        .iter()
                        .flat_map(|item| match item {
                            Item::Tool(tool) => tool.content.as_slice(),
                            _ => &[],
                        })
                        .filter_map(|content| match content {
                            ToolContent::Image(image) => Some(image),
                            _ => None,
                        }),
                )
            })
            .chain(incoming)
            .map(|preview| preview.rgba.len())
            .sum();
        for index in 0..self.node.turns.len() {
            if bytes <= clackatoo_protocol::ChatPreview::HISTORY_BYTES {
                break;
            }
            let turn = &mut self.node.turns[index];
            let mut changed = false;
            for item in &mut turn.items {
                if let Item::Tool(tool) = item {
                    tool.content.retain(|content| {
                        if let ToolContent::Image(image) = content {
                            bytes -= image.rgba.len();
                            changed = true;
                            false
                        } else {
                            true
                        }
                    });
                }
            }
            let previews = &mut turn.previews;
            if !previews.is_empty() {
                bytes -= previews
                    .iter()
                    .map(|preview| preview.rgba.len())
                    .sum::<usize>();
                previews.clear();
                changed = true;
            }
            if changed {
                self.patch_turn(index);
            }
        }
    }

    fn push_turn(&mut self, turn: Turn) {
        self.diffs.clear();
        self.node.turns.push(turn);
        self.patch_turn(self.node.turns.len() - 1);
    }

    /// Records that item `item` of turn `turn` is now `value`.
    fn patch_item(&mut self, turn: usize, item: usize, value: Item) {
        self.journal.push(ChatPatch::Item {
            turn: index_u32(turn),
            item: index_u32(item),
            value,
        });
    }

    /// Records that turn `index` changed, sending it whole.
    fn patch_turn(&mut self, index: usize) {
        self.journal.push(ChatPatch::Turn {
            index: index_u32(index),
            turn: self.node.turns[index].clone(),
        });
    }

    fn set_state(&mut self, state: ChatState) {
        if self.node.state != state {
            self.node.state = state.clone();
            self.journal.push(ChatPatch::State(state));
        }
    }

    fn set_question(&mut self, question: Option<Question>) {
        if self.node.question != question {
            self.node.question.clone_from(&question);
            self.journal.push(ChatPatch::Question(question));
        }
    }

    /// The agent's settings are `settings`, and the chat's own goes after
    /// them.
    fn set_settings(&mut self, mut settings: Vec<Setting>) {
        settings.push(tools_setting(self.tools()));
        settings.push(browser_setting(self.browser()));
        if self.node.settings != settings {
            self.node.settings.clone_from(&settings);
            self.journal.push(ChatPatch::Settings(settings));
        }
    }

    fn set_usage(&mut self, usage: Option<Usage>) {
        if self.node.usage != usage {
            self.node.usage = usage;
            self.journal.push(ChatPatch::Usage(usage));
        }
    }

    /// Forgets what the agent said of the context the chat fills, and of
    /// its account's limits, as the chat moves to another agent, whose
    /// account may be another.
    fn forget_usage(&mut self) {
        self.limits = Limits::default();
        self.set_usage(None);
    }

    pub(crate) fn set_provider_usage(&mut self, usage: Vec<clackatoo_protocol::ProviderUsage>) {
        if self.node.provider_usage != usage {
            self.node.provider_usage.clone_from(&usage);
            self.journal.push(ChatPatch::ProviderUsage(usage));
        }
    }

    /// Its agent's account's limits are now `limits`, as heard from any of
    /// that agent's chats: shown with the context it fills, once it's
    /// said how much that is.
    pub(crate) fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
        if let Some(usage) = self.node.usage {
            self.set_usage(Some(Usage {
                five_hour: limits.five_hour,
                weekly: limits.weekly,
                ..usage
            }));
        }
    }

    /// Says `notice` above the box, or nothing. It's no longer the one
    /// that says the agent is slow to open the session, if that was.
    fn set_notice(&mut self, notice: Option<String>) {
        self.slow = false;
        if self.node.notice != notice {
            self.node.notice.clone_from(&notice);
            self.journal.push(ChatPatch::Notice(notice));
        }
    }
}

/// The steps of the agent's `plan`.
fn steps(plan: agent_client_protocol_schema::v1::Plan) -> Vec<Step> {
    use agent_client_protocol_schema::v1::PlanEntryStatus;
    (plan.entries.into_iter())
        .map(|entry| Step {
            content: clean_text(&entry.content),
            state: match entry.status {
                PlanEntryStatus::Completed => StepState::Done,
                PlanEntryStatus::InProgress => StepState::Doing,
                // Pending, or a state newer than this build knows.
                _ => StepState::ToDo,
            },
        })
        .collect()
}

/// A tag for the lines a message or a digest goes between, that whoever
/// wrote what's between them can't know, so it can't end itself early and
/// go on as if clackatoo said what follows: from the clock's nanoseconds,
/// mixed with `salt`.
pub(crate) fn fence_tag(salt: u32) -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            since.subsec_nanos() ^ u32::try_from(since.as_secs() & 0xffff_ffff).unwrap_or(0)
        })
        ^ salt.rotate_left(16)
}

/// Whether `patch` adds to item `item` of turn `turn`.
fn adds_to(patch: &ChatPatch, turn: u32, item: u32) -> bool {
    matches!(patch, ChatPatch::Append { turn: t, item: i, .. } | ChatPatch::Output { turn: t, item: i, .. }
        if (*t, *i) == (turn, item))
}

/// Adds `patch` to `last`, if both add to the same item the same way.
/// Returns whether it did.
fn merge(last: &mut ChatPatch, patch: &ChatPatch) -> bool {
    match (last, patch) {
        (
            ChatPatch::Append { turn, item, text },
            ChatPatch::Append {
                turn: next_turn,
                item: next_item,
                text: next,
            },
        )
        | (
            ChatPatch::Output { turn, item, text },
            ChatPatch::Output {
                turn: next_turn,
                item: next_item,
                text: next,
            },
        ) if (*turn, *item) == (*next_turn, *next_item) => {
            text.push_str(next);
            true
        }
        _ => false,
    }
}

/// About how many bytes `patch` takes.
fn weight(patch: &ChatPatch) -> usize {
    let item = |item: &Item| match item {
        Item::Words(text) | Item::Thought(text) | Item::Other(text) => text.len(),
        Item::Tool(tool) => {
            let content: usize = (tool.content.iter())
                .map(|content| match content {
                    ToolContent::Text(text)
                    | ToolContent::Output(text)
                    | ToolContent::Other(text) => text.len(),
                    ToolContent::Image(image) => image.rgba.len(),
                    ToolContent::Diff(diff) => {
                        diff.path.len() + diff.old.as_ref().map_or(0, String::len) + diff.new.len()
                    }
                })
                .sum();
            let locations: usize = tool
                .locations
                .iter()
                .map(|location| location.path.len())
                .sum();
            tool.title.len() + content + locations
        }
    };
    let size = match patch {
        ChatPatch::Turn { turn, .. } => {
            turn.prompt.len()
                + turn.items.iter().map(item).sum::<usize>()
                + turn
                    .previews
                    .iter()
                    .map(|preview| preview.rgba.len())
                    .sum::<usize>()
        }
        ChatPatch::Delivery(delivery) => delivery.queued.iter().map(|p| p.text.len() + 128).sum(),
        ChatPatch::ProviderUsage(providers) => providers
            .iter()
            .map(|provider| {
                provider.agent.len()
                    + provider.title.len()
                    + provider.notice.as_ref().map_or(0, String::len)
                    + provider
                        .limits
                        .iter()
                        .map(|limit| limit.id.len() + limit.label.len() + 32)
                        .sum::<usize>()
                    + 32
            })
            .sum(),
        ChatPatch::Item { value, .. } => item(value),
        ChatPatch::Append { text, .. }
        | ChatPatch::Output { text, .. }
        | ChatPatch::Notice(Some(text)) => text.len(),
        ChatPatch::Question(Some(question)) => {
            question.title.len()
                + question
                    .choices
                    .iter()
                    .map(|choice| choice.name.len())
                    .sum::<usize>()
        }
        ChatPatch::Settings(settings) => (settings.iter())
            .flat_map(|setting| &setting.choices)
            .map(|choice| choice.name.len() + choice.value.len())
            .sum(),
        ChatPatch::Providers(providers) => (providers.iter())
            .flat_map(|provider| match &provider.models {
                ProviderModels::Known(choices) => choices.as_slice(),
                _ => &[],
            })
            .map(|choice| choice.name.len() + choice.value.len())
            .sum(),
        _ => 0,
    };
    // What each change takes, whatever's in it.
    size + 32
}

/// The chat's own setting for what its agent may do with clackatoo's
/// tools, at `tools`.
fn tools_setting(tools: AgentTools) -> Setting {
    Setting {
        id: TOOLS_SETTING.to_owned(),
        name: "Tools".to_owned(),
        kind: SettingKind::Tools,
        value: tools_value(tools).to_owned(),
        choices: (TOOLS_VALUES.iter())
            .map(|&(_, value, name, description)| SettingChoice {
                value: value.to_owned(),
                name: name.to_owned(),
                description: Some(description.to_owned()),
            })
            .collect(),
    }
}

/// The chat's own setting for whether its agent drives the browser, at
/// `browser`.
fn browser_setting(browser: bool) -> Setting {
    Setting {
        id: BROWSER_SETTING.to_owned(),
        name: "Browser".to_owned(),
        kind: SettingKind::Browser,
        value: if browser { "on" } else { "off" }.to_owned(),
        choices: (BROWSER_VALUES.iter())
            .map(|&(_, value, name, description)| SettingChoice {
                value: value.to_owned(),
                name: name.to_owned(),
                description: Some(description.to_owned()),
            })
            .collect(),
    }
}

/// How a chat keeps `tools` among the values the user gave settings.
fn tools_value(tools: AgentTools) -> &'static str {
    match tools {
        AgentTools::Off => "off",
        AgentTools::Ask => "ask",
        AgentTools::Allow => "allow",
    }
}

/// Now, in milliseconds since the Unix epoch.
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The settings the agent offers, as the chat shows them. Only those that
/// pick from a list: on/off ones aren't shown yet.
pub(crate) fn settings_of(options: &[SessionConfigOption]) -> Vec<Setting> {
    options
        .iter()
        .filter_map(|option| {
            let SessionConfigKind::Select(select) = &option.kind else {
                return None;
            };
            let listed: Vec<_> = match &select.options {
                SessionConfigSelectOptions::Ungrouped(choices) => choices.iter().collect(),
                SessionConfigSelectOptions::Grouped(groups) => {
                    groups.iter().flat_map(|group| &group.options).collect()
                }
                // A shape newer than this build knows.
                _ => Vec::new(),
            };
            Some(Setting {
                id: option.id.0.to_string(),
                name: clean(&option.name),
                kind: match option.category {
                    Some(SessionConfigOptionCategory::Mode) => SettingKind::Mode,
                    Some(SessionConfigOptionCategory::Model) => SettingKind::Model,
                    Some(SessionConfigOptionCategory::ThoughtLevel) => SettingKind::Effort,
                    _ => SettingKind::Other,
                },
                value: select.current_value.0.to_string(),
                choices: listed
                    .into_iter()
                    .map(|choice| SettingChoice {
                        value: choice.value.0.to_string(),
                        name: clean(&choice.name),
                        description: choice.description.as_deref().map(clean),
                    })
                    .collect(),
            })
        })
        .collect()
}

fn index_u32(index: usize) -> u32 {
    u32::try_from(index).expect("far fewer than 4 billion turns or items")
}

fn tool_status(status: ToolCallStatus) -> ToolStatus {
    match status {
        ToolCallStatus::Completed => ToolStatus::Done,
        ToolCallStatus::Failed => ToolStatus::Failed,
        _ => ToolStatus::Running,
    }
}

/// Whether a tool of `kind` may change files: an edit, or a command, as
/// running a program is, which says nothing of what it changed. A read,
/// search, thought or fetch changes nothing.
fn writes_files(kind: ToolKind) -> bool {
    matches!(
        kind,
        ToolKind::Edit | ToolKind::Execute | ToolKind::Delete | ToolKind::Move | ToolKind::Other
    )
}

/// Whether a tool's `input` names clackatoo's MCP server, as an MCP tool
/// call's does.
fn names_our_server(input: Option<&Value>) -> bool {
    input
        .and_then(|input| input.get("server").or_else(|| input.get("serverName")))
        .and_then(Value::as_str)
        == Some(crate::mcp::SERVER_NAME)
}

fn tool_kind(kind: agent_client_protocol_schema::v1::ToolKind) -> ToolKind {
    use agent_client_protocol_schema::v1::ToolKind as Acp;
    match kind {
        Acp::Read => ToolKind::Read,
        Acp::Edit => ToolKind::Edit,
        Acp::Delete => ToolKind::Delete,
        Acp::Move => ToolKind::Move,
        Acp::Search => ToolKind::Search,
        Acp::Execute => ToolKind::Execute,
        Acp::Think => ToolKind::Think,
        Acp::Fetch => ToolKind::Fetch,
        // Switching mode, or a kind newer than this build knows.
        _ => ToolKind::Other,
    }
}

/// What a tool shows, as the agent gives it: its text and its changes to
/// files, cut down to what a chat keeps. Image labels arrive here first;
/// the bounded worker attaches their pixels separately.
fn tool_content(content: &[ToolCallContent]) -> Vec<ToolContent> {
    (content.iter())
        .filter_map(|content| match content {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Text(text) => {
                    Some(ToolContent::Text(tail(&clean_text(&text.text), TOOL_TEXT)))
                }
                ContentBlock::Image(_)
                | ContentBlock::Resource(_)
                | ContentBlock::ResourceLink(_) => Some(ToolContent::Other(
                    "Image or resource returned by tool".into(),
                )),
                _ => None,
            },
            ToolCallContent::Diff(diff) => Some(ToolContent::Diff(Diff {
                path: diff.path.to_string_lossy().into_owned(),
                old: (diff.old_text.as_deref()).map(|old| head(&clean_text(old), DIFF_TEXT)),
                new: head(&clean_text(&diff.new_text), DIFF_TEXT),
            })),
            _ => None,
        })
        .collect()
}

fn tool_locations(locations: &[ToolCallLocation]) -> Vec<Location> {
    (locations.iter())
        .map(|location| Location {
            path: location.path.to_string_lossy().into_owned(),
            line: location.line,
        })
        .collect()
}

/// The last whole lines of `text` that fit in `max` bytes, after "…", if
/// it's longer. A fence it opens with stays, for it to still read as code.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let fence = if text.starts_with("```") {
        text.split_inclusive('\n').next().unwrap_or_default()
    } else {
        ""
    };
    let mut start = text.len() - max.saturating_sub(fence.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let rest = &text[start..];
    // Cut in a line: from the next.
    let rest = if text.as_bytes()[start - 1] == b'\n' {
        rest
    } else {
        rest.split_once('\n').map_or(rest, |(_, after)| after)
    };
    format!("{fence}…\n{rest}")
}

/// The first whole lines of `text` that fit in `max` bytes, if it's
/// longer.
fn head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &text[..end];
    cut.rfind('\n').map_or(cut, |line| &cut[..=line]).to_owned()
}

/// A title or name on one line, without control characters.
fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .filter(|ch| !ch.is_control())
        .collect()
}

/// The agent's words without control characters, but with their line
/// breaks and tabs.
fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|&ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        .collect()
}

fn setting_to_handover(setting: Setting) -> handover::ChatSetting {
    handover::ChatSetting {
        id: setting.id,
        name: setting.name,
        kind: match setting.kind {
            SettingKind::Mode => handover::SettingKind::Mode,
            SettingKind::Model => handover::SettingKind::Model,
            SettingKind::Effort => handover::SettingKind::Effort,
            // Never handed over: the chat keeps its value with those chosen.
            SettingKind::Other | SettingKind::Tools | SettingKind::Browser => {
                handover::SettingKind::Other
            }
        },
        value: setting.value,
        choices: (setting.choices.into_iter())
            .map(|choice| handover::ChatChoice {
                value: choice.value,
                name: choice.name,
                description: choice.description,
            })
            .collect(),
    }
}

fn setting_from_handover(setting: handover::ChatSetting) -> Setting {
    Setting {
        id: setting.id,
        name: setting.name,
        kind: match setting.kind {
            handover::SettingKind::Mode => SettingKind::Mode,
            handover::SettingKind::Model => SettingKind::Model,
            handover::SettingKind::Effort => SettingKind::Effort,
            handover::SettingKind::Other => SettingKind::Other,
        },
        value: setting.value,
        choices: (setting.choices.into_iter())
            .map(|choice| SettingChoice {
                value: choice.value,
                name: choice.name,
                description: choice.description,
            })
            .collect(),
    }
}

fn turn_to_handover(turn: Turn) -> handover::ChatTurn {
    handover::ChatTurn {
        prompt: turn.prompt,
        sent: turn.sent,
        items: (turn.items.into_iter())
            .map(|item| match item {
                // The daemon makes no Other: it's what a newer one sends.
                Item::Words(words) | Item::Other(words) => handover::ChatItem::Words(words),
                Item::Thought(words) => handover::ChatItem::Thought(words),
                Item::Tool(tool) => handover::ChatItem::Tool {
                    id: tool.id,
                    title: tool.title,
                    status: match tool.status {
                        ToolStatus::Running => handover::ToolStatus::Running,
                        ToolStatus::Done => handover::ToolStatus::Done,
                        ToolStatus::Failed => handover::ToolStatus::Failed,
                    },
                    kind: match tool.kind {
                        ToolKind::Read => handover::ToolKind::Read,
                        ToolKind::Edit => handover::ToolKind::Edit,
                        ToolKind::Delete => handover::ToolKind::Delete,
                        ToolKind::Move => handover::ToolKind::Move,
                        ToolKind::Search => handover::ToolKind::Search,
                        ToolKind::Execute => handover::ToolKind::Execute,
                        ToolKind::Think => handover::ToolKind::Think,
                        ToolKind::Fetch => handover::ToolKind::Fetch,
                        ToolKind::Other => handover::ToolKind::Other,
                    },
                    content: (tool.content.into_iter())
                        .map(|content| match content {
                            ToolContent::Text(text) | ToolContent::Other(text) => {
                                handover::ToolContent::Text(text)
                            }
                            ToolContent::Output(text) => handover::ToolContent::Output(text),
                            ToolContent::Image(_) => {
                                handover::ToolContent::Text("Image preview".into())
                            }
                            ToolContent::Diff(diff) => handover::ToolContent::Diff {
                                path: diff.path,
                                old: diff.old,
                                new: diff.new,
                            },
                        })
                        .collect(),
                    locations: (tool.locations.into_iter())
                        .map(|location| handover::ToolLocation {
                            path: location.path,
                            line: location.line,
                        })
                        .collect(),
                },
            })
            .collect(),
        end: turn.end.map(|end| handover::ChatEnd {
            at: end.at,
            outcome: match end.outcome {
                Outcome::Done => handover::ChatOutcome::Done,
                Outcome::Stopped => handover::ChatOutcome::Stopped,
                Outcome::Failed(why) => handover::ChatOutcome::Failed(why),
            },
        }),
    }
}

fn turn_from_handover(turn: handover::ChatTurn) -> Turn {
    Turn {
        previews: Vec::new(),
        prompt: turn.prompt,
        sent: turn.sent,
        items: (turn.items.into_iter())
            .map(|item| match item {
                handover::ChatItem::Words(words) => Item::Words(words),
                handover::ChatItem::Thought(words) => Item::Thought(words),
                handover::ChatItem::Tool {
                    id,
                    title,
                    status,
                    kind,
                    content,
                    locations,
                } => Item::Tool(Tool {
                    id,
                    title,
                    status: match status {
                        handover::ToolStatus::Running => ToolStatus::Running,
                        handover::ToolStatus::Done => ToolStatus::Done,
                        handover::ToolStatus::Failed => ToolStatus::Failed,
                    },
                    kind: match kind {
                        handover::ToolKind::Read => ToolKind::Read,
                        handover::ToolKind::Edit => ToolKind::Edit,
                        handover::ToolKind::Delete => ToolKind::Delete,
                        handover::ToolKind::Move => ToolKind::Move,
                        handover::ToolKind::Search => ToolKind::Search,
                        handover::ToolKind::Execute => ToolKind::Execute,
                        handover::ToolKind::Think => ToolKind::Think,
                        handover::ToolKind::Fetch => ToolKind::Fetch,
                        handover::ToolKind::Other => ToolKind::Other,
                    },
                    content: (content.into_iter())
                        .map(|content| match content {
                            handover::ToolContent::Text(text) => ToolContent::Text(text),
                            handover::ToolContent::Output(text) => ToolContent::Output(text),
                            handover::ToolContent::Diff { path, old, new } => {
                                ToolContent::Diff(Diff { path, old, new })
                            }
                        })
                        .collect(),
                    locations: (locations.into_iter())
                        .map(|location| Location {
                            path: location.path,
                            line: location.line,
                        })
                        .collect(),
                }),
            })
            .collect(),
        end: turn.end.map(|end| TurnEnd {
            at: end.at,
            outcome: match end.outcome {
                handover::ChatOutcome::Done => Outcome::Done,
                handover::ChatOutcome::Stopped => Outcome::Stopped,
                handover::ChatOutcome::Failed(why) => Outcome::Failed(why),
            },
        }),
        files: None,
        // Put back from the chat's relations, if another agent sent it.
        from: None,
        switch: None,
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol_schema::v1::{RequestPermissionRequest, SessionNotification};

    use super::*;

    #[test]
    fn user_queue_orders_bounds_and_removes_without_reusing_ids() {
        let mut c = chat();
        assert!(c.enqueue_user(" ".into(), vec![], None).is_err());
        assert!(c.enqueue_user("x".repeat(65537), vec![], None).is_err());
        let first = c.enqueue_user("first".into(), vec![], None).unwrap();
        for n in 1..8 {
            c.enqueue_user(format!("message {n}"), vec![], None)
                .unwrap();
        }
        assert!(c.enqueue_user("overflow".into(), vec![], None).is_err());
        assert_eq!(c.next_user(), Some(first));
        assert_eq!(c.take_queued_user(first).unwrap().prompt.text, "first");
        assert!(c.take_queued_user(first).is_err());
        assert_eq!(c.enqueue_user("last".into(), vec![], None).unwrap(), 9);
        assert_eq!(c.next_user(), Some(2));
    }

    #[test]
    fn user_queue_waits_for_completion_and_pauses_on_stop_or_failure() {
        let mut c = chat();
        connect(&mut c, "s");
        send(&mut c, "work");
        let id = c.enqueue_user("next".into(), vec![], None).unwrap();
        assert_eq!(c.next_user(), None);
        c.answered(Ok(StopReason::EndTurn));
        assert_eq!(c.next_user(), Some(id));
        c.act(ChatAction::Stop);
        assert_eq!(c.next_user(), None);
        c.pause_queue(false);
        send(&mut c, "more");
        c.answered(Err("failed".into()));
        assert_eq!(c.next_user(), None);
        assert!(c.queued_user(id).is_some());
    }

    #[test]
    fn steering_restart_waits_for_cancel_and_manual_stop_revokes_it() {
        let mut c = chat();
        connect(&mut c, "s");
        send(&mut c, "work");
        let id = c.enqueue_user("redirect".into(), vec![], None).unwrap();
        assert_eq!(c.steer_restart(id), vec![Action::Cancel]);
        assert_eq!(c.next_user(), None);
        c.answered(Ok(StopReason::Cancelled));
        assert_eq!(c.next_user(), Some(id));
        c.fail_queue("Provider removed while cancellation was pending".into());
        assert_eq!(c.next_user(), None);
        c.act(ChatAction::Stop);
        assert_eq!(c.next_user(), None);
        send(&mut c, "again");
        c.steer_restart(id);
        c.answered(Err("connection failed".into()));
        assert_eq!(c.next_user(), None);
    }

    #[test]
    fn native_steering_tracks_the_original_turn_and_blocks_queue_until_ack() {
        for early_completion in [false, true] {
            let mut c = chat();
            connect(&mut c, "s");
            send(&mut c, "work");
            let id = c.enqueue_user("redirect".into(), vec![], None).unwrap();
            let next = c.enqueue_user("later".into(), vec![], None).unwrap();
            assert_eq!(c.begin_steering(id).unwrap(), "redirect");
            assert!(c.take_queued_user(id).is_err());
            if early_completion {
                c.answered(Ok(StopReason::EndTurn));
            }
            assert_eq!(c.next_user(), None);
            c.steered(id, Ok("injected".into()));
            assert!(c.queued_user(id).is_none());
            if !early_completion {
                c.answered(Ok(StopReason::EndTurn));
            }
            assert_eq!(c.next_user(), Some(next));
            assert!(
                matches!(&c.node.turns[0].items[0], Item::Other(t) if t.contains("You · Steering") && !t.contains("sending"))
            );
        }
    }

    #[test]
    fn native_steering_idle_race_retries_only_when_definitely_unconsumed() {
        for outcome in ["promptRequired", "startedNewTurn", "bad"] {
            let mut c = chat();
            connect(&mut c, "s");
            send(&mut c, "work");
            let id = c.enqueue_user("redirect".into(), vec![], None).unwrap();
            c.begin_steering(id).unwrap();
            c.answered(Ok(StopReason::EndTurn));
            c.steered(id, Ok(outcome.into()));
            assert_eq!(c.next_user(), (outcome == "promptRequired").then_some(id));
            assert!(c.queued_user(id).is_some());
        }
    }

    #[test]
    fn native_steering_stop_and_disconnect_do_not_restart_work() {
        let mut c = chat();
        connect(&mut c, "s");
        send(&mut c, "work");
        let id = c.enqueue_user("redirect".into(), vec![], None).unwrap();
        c.begin_steering(id).unwrap();
        c.act(ChatAction::Stop);
        c.steered(id, Ok("promptRequired".into()));
        c.answered(Ok(StopReason::Cancelled));
        assert_eq!(c.next_user(), None);
        c.pause_queue(false);
        send(&mut c, "again");
        c.begin_steering(id).unwrap();
        c.disconnected(None);
        assert_eq!(c.next_user(), None);
        assert_eq!(c.node.delivery.sending, None);
        assert!(c.queued_user(id).is_some());
    }

    #[test]
    fn queued_payloads_and_ids_survive_upgrade_but_never_auto_run() {
        let mut c = chat();
        let image = clackatoo_protocol::ChatImage {
            data: "cG5n".into(),
            width: 1,
            height: 1,
            preview: Some(clackatoo_protocol::ChatPreview {
                width: 1,
                height: 1,
                rgba: vec![1, 2, 3, 255].into(),
                original: 0,
            }),
        };
        let target = clackatoo_protocol::ChatTarget {
            agent: "codex".into(),
            model: Some("model".into()),
            budget: 12000,
        };
        let id = c
            .enqueue_user("next".into(), vec![image.clone()], Some(target.clone()))
            .unwrap();
        let record = c.hand_over_queue();
        let mut resumed = chat();
        resumed.resume_queue(&mut [record]);
        assert_eq!(resumed.next_user(), None);
        let kept = resumed.queued_user(id).unwrap();
        assert_eq!(kept.prompt.images, vec![image]);
        assert_eq!(kept.target, Some(target));
        assert!(resumed.enqueue_user("later".into(), vec![], None).unwrap() > id);
    }

    fn chat() -> HostedChat {
        chat_with("claude")
    }

    /// A chat with the known agent called `name`.
    fn chat_with(name: &str) -> HostedChat {
        let agent = crate::agents::AgentConfig::default().find(name).unwrap();
        HostedChat::new(
            TerminalId(1),
            Size::new(40, 12).unwrap(),
            "/tmp".to_owned(),
            &agent,
            AgentTools::Ask,
            true,
            false,
        )
    }

    /// Answers the question `chat` shows with the choice `option`.
    fn answer(chat: &mut HostedChat, option: Option<String>) -> Vec<Action> {
        let shown = chat.node.question.as_ref();
        let question = shown.map_or(u64::MAX, |question| question.id);
        chat.act(ChatAction::Answer { question, option })
    }

    fn update(chat: &mut HostedChat, json: &str) -> Option<Wrote> {
        let json = format!(r#"{{"sessionId":"s","update":{json}}}"#);
        let notification: SessionNotification = serde_json::from_str(&json).unwrap();
        chat.update(notification.update)
    }

    #[test]
    fn previews_survive_handover_and_recent_images_have_a_bounded_budget() {
        let mut chat = chat();
        let preview = clackatoo_protocol::ChatPreview {
            original: 0,
            width: 256,
            height: 256,
            rgba: vec![42; 256 * 256 * 4].into(),
        };
        for index in 0..20 {
            let prompt = Prompt {
                text: format!("image {index}"),
                images: vec![clackatoo_protocol::ChatImage {
                    data: "cG5n".into(),
                    width: 256,
                    height: 256,
                    preview: Some(preview.clone()),
                }],
            };
            chat.begin(prompt.shown(), prompt, None, &mut Vec::new());
        }
        assert!(chat.node.turns[0].previews.is_empty());
        assert!(chat.node.turns[0].prompt.contains("Image"));
        assert_eq!(chat.node.turns[19].previews[0].rgba, preview.rgba);
        assert_eq!(chat.node.turns[19].previews[0].original, 20);
        let bytes: usize = chat
            .node
            .turns
            .iter()
            .flat_map(|turn| &turn.previews)
            .map(|p| p.rgba.len())
            .sum();
        assert_eq!(bytes, clackatoo_protocol::ChatPreview::HISTORY_BYTES);
        let previews = chat.hand_over_previews();
        let mut originals = vec![chat.hand_over_originals()];
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let mut resumed = HostedChat::resume(chat.hand_over(), &agent);
        resumed.resume_previews(previews);
        resumed.resume_originals(&mut originals);
        assert_eq!(resumed.node.turns[19].previews[0].rgba, preview.rgba);
        assert_eq!(
            resumed
                .originals
                .get(resumed.node.turns[19].previews[0].original),
            Some("cG5n".into())
        );
        assert!(resumed.node.turns[0].previews.is_empty());
    }

    #[test]
    fn tool_previews_reject_stale_results_and_survive_handover() {
        let mut chat = chat();
        chat.begin(
            "look".into(),
            Prompt {
                text: "look".into(),
                images: Vec::new(),
            },
            None,
            &mut Vec::new(),
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"image","title":"View Image","kind":"read","status":"completed"}"#,
        );
        let stale = chat.image_target("image").unwrap();
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"image","content":[]}"#,
        );
        assert!(
            !chat
                .image_receipts
                .iter()
                .any(|(_, receipt)| receipt.ptr_eq(&std::sync::Arc::downgrade(&stale.receipt)))
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"image","content":[{"type":"content","content":{"type":"text","text":"replaced"}}]}"#,
        );
        let preview = clackatoo_protocol::ChatPreview {
            original: 0,
            width: 1,
            height: 1,
            rgba: vec![20, 40, 60, 255].into(),
        };
        assert!(!chat.tool_originals(&stale, vec![(preview.clone(), "cG5n".into())]));
        assert!(chat.originals.get(1).is_none());
        let target = chat.image_target("image").unwrap();
        assert!(chat.tool_originals(&target, vec![(preview.clone(), "cG5n".into())]));
        let mut originals = vec![chat.hand_over_originals()];
        let mut images = chat.hand_over_tool_previews();
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let mut resumed = HostedChat::resume(chat.hand_over(), &agent);
        resumed.resume_tool_previews(&mut images);
        resumed.resume_originals(&mut originals);
        let preview = clackatoo_protocol::ChatPreview {
            original: 1,
            ..preview
        };
        assert_eq!(resumed.originals.get(1), Some("cG5n".into()));
        assert!(resumed.node.turns[0].items.iter().any(|item| matches!(item, Item::Tool(tool) if tool.content.contains(&ToolContent::Image(preview.clone())))));
    }

    #[test]
    fn prompt_and_tool_previews_share_the_history_budget() {
        let mut chat = chat();
        let preview = clackatoo_protocol::ChatPreview {
            original: 0,
            width: 256,
            height: 256,
            rgba: vec![42; 256 * 256 * 4].into(),
        };
        for index in 0..20 {
            let mut prompt = image_prompt();
            prompt.text = format!("image {index}");
            prompt.images[0].preview = Some(preview.clone());
            chat.begin(prompt.shown(), prompt, None, &mut Vec::new());
            update(
                &mut chat,
                r#"{"sessionUpdate":"tool_call","toolCallId":"image","title":"View Image","kind":"read","status":"completed"}"#,
            );
            let target = chat.image_target("image").unwrap();
            assert!(chat.tool_images(&target, vec![preview.clone()]));
        }
        let prompts: usize = chat
            .node
            .turns
            .iter()
            .flat_map(|turn| &turn.previews)
            .map(|image| image.rgba.len())
            .sum();
        let tools: usize = chat
            .hand_over_tool_previews()
            .iter()
            .flat_map(|record| &record.images)
            .map(|image| image.rgba.len())
            .sum();
        assert_eq!(
            prompts + tools,
            clackatoo_protocol::ChatPreview::HISTORY_BYTES
        );
        assert_eq!(prompts, tools);
        assert!(chat.node.turns[0].previews.is_empty());
        assert_eq!(chat.node.turns[19].previews[0].rgba, preview.rgba);
        assert_eq!(chat.node.turns[19].previews[0].original, 20);
        assert_eq!(chat.hand_over_tool_previews()[0].turn, 12);
    }

    #[test]
    fn malformed_preview_is_refused_before_starting_a_turn() {
        let mut chat = chat();
        let mut prompt = image_prompt();
        prompt.images[0].preview = Some(clackatoo_protocol::ChatPreview {
            original: 0,
            width: u32::MAX,
            height: 1,
            rgba: vec![0; 4].into(),
        });
        assert!(
            chat.act(ChatAction::SendImages {
                text: prompt.text,
                images: prompt.images
            })
            .is_empty()
        );
        assert!(chat.node.turns.is_empty());
        assert!(chat.node.notice.is_some());
    }

    fn image_prompt() -> Prompt {
        Prompt {
            text: "look".into(),
            images: vec![clackatoo_protocol::ChatImage {
                preview: None,
                data: "cG5n".into(),
                width: 2,
                height: 3,
            }],
        }
    }

    #[test]
    fn images_survive_opening_and_loading_and_a_stop_drops_them() {
        let prompt = image_prompt();
        let mut chat = chat();
        chat.send_prompt(prompt.clone(), &mut Vec::new());
        assert_eq!(connect(&mut chat, "s"), [Action::Prompt(prompt.clone())]);
        assert_eq!(chat.node.turns[0].prompt, "look\n\n[Image · 2 × 3]");
        let mut loading = resuming();
        replay_user(&mut loading, "before");
        loading.send_prompt(prompt.clone(), &mut Vec::new());
        send(&mut loading, "also this");
        loading.opened_again(true);
        let actions = connect(&mut loading, "past");
        assert!(
            matches!(&actions[..], [Action::Prompt(sent)] if sent.images == prompt.images && sent.text == "look\n\nalso this")
        );
        let mut stopped = resuming();
        stopped.send_prompt(prompt, &mut Vec::new());
        stopped.act(ChatAction::Stop);
        assert!(stopped.queued.is_none());
        assert!(
            connect(&mut stopped, "past")
                .iter()
                .all(|action| !matches!(action, Action::Prompt(_)))
        );
    }

    #[test]
    fn a_finished_edit_says_what_it_wrote_and_whatever_is_running_is_known() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "edit it");
        let wrote = |paths: &[&str], diffs: Vec<RawDiff>| Wrote {
            done: true,
            turn: 0,
            tool: "e".into(),
            order: 0,
            paths: paths.iter().map(|&path| path.to_owned()).collect(),
            diffs,
        };
        // Asking first, as Claude Code's adapter does: nothing written yet,
        // but the file it's about to change is named.
        let pending = r#"{"sessionUpdate":"tool_call","toolCallId":"e","title":"Edit","kind":"edit",
                          "status":"pending","locations":[{"path":"/a"}]}"#;
        let about_to = Wrote {
            done: false,
            ..wrote(&["/a"], Vec::new())
        };
        assert_eq!(update(&mut chat, pending), Some(about_to));
        assert_eq!(chat.working_on(), (0, Some("e".to_owned())));
        // Finished, then what it wrote, each saying so.
        let done = r#"{"sessionUpdate":"tool_call_update","toolCallId":"e","status":"completed"}"#;
        assert_eq!(update(&mut chat, done), Some(wrote(&["/a"], Vec::new())));
        assert_eq!(chat.working_on(), (0, None));
        let content = r#"{"sessionUpdate":"tool_call_update","toolCallId":"e",
                          "content":[{"type":"diff","path":"/b","oldText":"x","newText":"y"}],
                          "locations":[{"path":"/b","line":1},{"path":"/a"}]}"#;
        // What it says it changed, kept whole, from then on.
        let diffs = vec![RawDiff {
            path: "/b".into(),
            old: Some("x".into()),
            new: "y".into(),
        }];
        let both = wrote(&["/b", "/a"], diffs.clone());
        assert_eq!(update(&mut chat, content), Some(both));
        let done_again =
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"e","status":"completed"}"#;
        assert_eq!(
            update(&mut chat, done_again),
            Some(wrote(&["/b", "/a"], diffs))
        );
        // An update saying nothing of it says nothing again.
        let title = r#"{"sessionUpdate":"tool_call_update","toolCallId":"e","title":"Edited"}"#;
        assert_eq!(update(&mut chat, title), None);
        // A command that finishes wrote nothing it says.
        let command = r#"{"sessionUpdate":"tool_call","toolCallId":"c","title":"ls",
                          "kind":"execute","status":"completed"}"#;
        assert_eq!(update(&mut chat, command), None);
    }

    fn words(chat: &mut HostedChat, text: &str) {
        let text = serde_json::to_string(text).unwrap();
        update(
            chat,
            &format!(
                r#"{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":{text}}}}}"#
            ),
        );
    }

    /// Opens `chat`'s session, `session`, with no settings.
    fn connect(chat: &mut HostedChat, session: &str) -> Vec<Action> {
        chat.connected(Opened {
            session: session.to_owned(),
            settings: None,
        })
    }

    /// Settings as Claude Code's adapter gives them: its mode, Manual, and
    /// its model, `model`.
    fn claude_settings(model: &str) -> Vec<SessionConfigOption> {
        serde_json::from_str(&format!(
            r#"[{{"id":"mode","name":"Mode","category":"mode","type":"select","currentValue":"default",
                  "options":[{{"value":"default","name":"Manual"}},{{"value":"auto","name":"Auto"}}]}},
                {{"id":"model","name":"Model","category":"model","type":"select","currentValue":"{model}",
                  "options":[{{"value":"haiku","name":"Haiku"}},{{"value":"sonnet","name":"Sonnet"}}]}}]"#
        ))
        .unwrap()
    }

    fn send(chat: &mut HostedChat, text: &str) -> Vec<Action> {
        chat.act(ChatAction::Send(text.to_owned()))
    }

    fn outcome(turn: &Turn) -> Option<&Outcome> {
        turn.end.as_ref().map(|end| &end.outcome)
    }

    /// A viewer's copy of the chat, kept with the updates it's sent.
    struct Viewer {
        sent: ChatSent,
        view: Option<View>,
    }

    impl Viewer {
        fn new() -> Self {
            Self {
                sent: ChatSent::default(),
                view: None,
            }
        }

        /// Catches up, returning the update it got.
        fn catch_up(&mut self, chat: &HostedChat) -> Option<ViewUpdate> {
            let update = chat.catch_up(&mut self.sent)?;
            match (&mut self.view, update.clone()) {
                (Some(view), update) => view.apply(update),
                (None, ViewUpdate::Snapshot(view)) => self.view = Some(view),
                (None, ViewUpdate::Patch(_)) => panic!("a patch before a snapshot"),
            }
            Some(update)
        }
    }

    #[test]
    fn a_message_waits_for_the_session_then_goes() {
        let mut chat = chat();
        assert_eq!(chat.node.state, ChatState::Starting);
        // Sent before the session is made: it waits.
        assert_eq!(send(&mut chat, "hello"), []);
        assert_eq!(chat.node.turns[0].prompt, "hello");
        assert_eq!(connect(&mut chat, "s1"), [Action::Prompt("hello".into())]);
        assert_eq!(
            (chat.session(), &chat.node.state),
            (Some("s1"), &ChatState::Answering)
        );
        chat.answered(Ok(StopReason::EndTurn));
        // Sent once there's a session: it goes at once.
        assert_eq!(send(&mut chat, "again"), [Action::Prompt("again".into())]);
        // And not while it's being answered.
        assert_eq!(send(&mut chat, "more"), []);
    }

    #[test]
    fn words_stream_into_the_turn_and_tools_go_between_them() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "hi");
        words(&mut chat, "Let me ");
        words(&mut chat, "look.\n");
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"t1","title":"Read a.rs","status":"in_progress"}"#,
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"}"#,
        );
        words(&mut chat, "Done.");
        chat.answered(Ok(StopReason::EndTurn));
        let turn = &chat.node.turns[0];
        assert_eq!(
            turn.items,
            [
                Item::Words("Let me look.\n".into()),
                Item::Tool(Tool {
                    id: "t1".into(),
                    title: "Read a.rs".into(),
                    status: ToolStatus::Done,
                    kind: ToolKind::Other,
                    content: Vec::new(),
                    locations: Vec::new(),
                }),
                Item::Words("Done.".into()),
            ]
        );
        assert_eq!(outcome(turn), Some(&Outcome::Done));
    }

    #[test]
    fn a_tool_keeps_what_it_shows_which_each_update_replaces() {
        let mut chat = chat();
        chat.act(ChatAction::Send("run it".into()));
        let mut viewer = Viewer::new();
        viewer.catch_up(&chat);
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"b","title":"ls","kind":"execute","status":"pending",
                "content":[{"type":"content","content":{"type":"text","text":"List files"}}]}"#,
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"b","status":"completed",
                "content":[{"type":"content","content":{"type":"text","text":"```console\na\n```"}}]}"#,
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"w","title":"Write a","kind":"edit","status":"pending",
                "content":[{"type":"diff","path":"/src/a","oldText":null,"newText":"x\n"}]}"#,
        );
        let tools: Vec<(ToolKind, &[ToolContent])> = (chat.node.turns[0].items.iter())
            .filter_map(|item| match item {
                Item::Tool(tool) => Some((tool.kind, tool.content.as_slice())),
                _ => None,
            })
            .collect();
        assert_eq!(
            tools,
            [
                (
                    ToolKind::Execute,
                    &[ToolContent::Text("```console\na\n```".into())][..]
                ),
                (
                    ToolKind::Edit,
                    &[ToolContent::Diff(Diff {
                        path: "/src/a".into(),
                        old: None,
                        new: "x\n".into(),
                    })][..]
                ),
            ]
        );
        // Each change goes as the tool alone, not its turn.
        let Some(ViewUpdate::Patch(patches)) = viewer.catch_up(&chat) else {
            panic!("no patches");
        };
        assert!(
            (patches.iter()).all(|patch| matches!(patch, Patch::Chat(ChatPatch::Item { .. }))),
            "{patches:?}"
        );
    }

    #[test]
    fn a_long_output_keeps_its_end_and_a_long_diff_its_start() {
        let output = format!("```console\n{}```", "line\n".repeat(10));
        let kept = tail(&output, 24);
        assert_eq!(kept, "```console\n…\nline\nline\n```");
        assert_eq!(tail("short", 24), "short");
        assert_eq!(head(&"line\n".repeat(10), 12), "line\nline\n");
        // Never in the middle of a character.
        assert_eq!(tail("ééééé\nab", 3), "…\nab");
        assert_eq!(head("ééé", 3), "é");
    }

    #[test]
    fn viewers_get_the_chat_then_its_changes_with_streamed_words_as_one() {
        let mut chat = chat();
        let mut viewer = Viewer::new();
        assert!(matches!(
            viewer.catch_up(&chat),
            Some(ViewUpdate::Snapshot(_))
        ));
        assert_eq!(viewer.catch_up(&chat), None);
        connect(&mut chat, "s");
        send(&mut chat, "hi");
        words(&mut chat, "Hel");
        words(&mut chat, "lo");
        words(&mut chat, " there");
        let Some(ViewUpdate::Patch(patches)) = viewer.catch_up(&chat) else {
            panic!("patches");
        };
        let appends: Vec<&Patch> = (patches.iter())
            .filter(|patch| matches!(patch, Patch::Chat(ChatPatch::Append { .. })))
            .collect();
        assert_eq!(
            appends,
            [&Patch::Chat(ChatPatch::Append {
                turn: 0,
                item: 0,
                text: "Hello there".into(),
            })]
        );
        assert_eq!(viewer.view.as_ref(), Some(&chat.view()));
    }

    #[test]
    fn a_viewer_too_far_behind_gets_the_chat_whole() {
        let mut chat = chat();
        connect(&mut chat, "s");
        let mut viewer = Viewer::new();
        viewer.catch_up(&chat);
        send(&mut chat, "hi");
        for _ in 0..=JOURNAL {
            words(&mut chat, "x");
        }
        assert!(matches!(
            viewer.catch_up(&chat),
            Some(ViewUpdate::Snapshot(_))
        ));
        assert_eq!(viewer.view.as_ref(), Some(&chat.view()));
    }

    #[test]
    fn the_agent_asks_and_the_answer_goes_back() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        let options: Vec<PermissionOption> = serde_json::from_str(
            r#"[{"optionId":"yes","name":"Allow","kind":"allow_once"},
                {"optionId":"no","name":"Reject","kind":"reject_once"}]"#,
        )
        .unwrap();
        let tool: ToolCallUpdate =
            serde_json::from_str(r#"{"toolCallId":"t","title":"run\nls"}"#).unwrap();
        chat.ask(Value::from(7), &tool, &options);
        let question = chat.node.question.clone().expect("a question");
        assert_eq!(question.title, "run ls");
        assert_eq!(question.choices[1].kind, ChoiceKind::RejectOnce);
        assert_eq!(
            answer(&mut chat, Some("yes".into())),
            [Action::Answer {
                request: Value::from(7),
                option: Some("yes".into()),
            }]
        );
        assert_eq!(chat.node.question, None);
        // Nothing asked: an answer goes nowhere.
        assert_eq!(answer(&mut chat, None), []);
    }

    #[test]
    fn questions_wait_their_turn_and_tools_keep_theirs_past_the_turn() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        let tool: ToolCallUpdate =
            serde_json::from_str(r#"{"toolCallId":"t","title":"Write a"}"#).unwrap();
        chat.ask(Value::from(1), &tool, &[]);
        chat.ask_for_tool(7, "run `make` in a terminal", None, Choices::Tool);
        chat.ask_for_tool(8, "run `ls` in a terminal", None, Choices::Tool);
        assert_eq!(chat.node.question.as_ref().unwrap().title, "Write a");
        assert_eq!(
            answer(&mut chat, Some("x".into())),
            [Action::Answer {
                request: Value::from(1),
                option: Some("x".into()),
            }]
        );
        let question = chat.node.question.clone().unwrap();
        assert_eq!(question.title, "run `make` in a terminal");
        assert_eq!(question.choices[1].kind, ChoiceKind::AllowAlways);
        // A call cancelled takes its question back.
        chat.withdraw(7);
        let title = &chat.node.question.as_ref().unwrap().title;
        assert_eq!(title, "run `ls` in a terminal");
        // The turn ending leaves it: the call still waits.
        chat.answered(Ok(StopReason::EndTurn));
        assert!(chat.node.question.is_some());
        assert_eq!(chat.tools(), AgentTools::Ask);
        assert_eq!(
            answer(&mut chat, Some("always".into())),
            [Action::Permit {
                call: 8,
                allow: true,
                pair: false,
            }]
        );
        assert_eq!(
            chat.tools(),
            AgentTools::Allow,
            "always allowed from then on"
        );
        assert_eq!(chat.node.question, None);
        // Kept through an upgrade, and never sent to the agent.
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let mut resumed = HostedChat::resume(chat.hand_over(), &agent);
        assert_eq!(resumed.tools(), AgentTools::Allow);
        let opened = Opened {
            session: String::new(),
            settings: None,
        };
        assert_eq!(resumed.connected(opened), []);
    }

    #[test]
    fn codexs_question_about_clackatoos_own_tools_is_answered_at_once() {
        let mut chat = chat_with("codex");
        connect(&mut chat, "s");
        send(&mut chat, "go");
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"m","title":"mcp.clackatoo.run_in_terminal",
                "kind":"execute","status":"pending","rawInput":{"server":"clackatoo"}}"#,
        );
        let request: RequestPermissionRequest = serde_json::from_str(
            r#"{"sessionId":"s","toolCall":{"toolCallId":"m","kind":"execute"},
                "options":[{"optionId":"approved","name":"Allow","kind":"allow_once"},
                           {"optionId":"cancel","name":"Cancel","kind":"reject_once"}]}"#,
        )
        .unwrap();
        let allowed = chat.about_own_tools(&request.tool_call, &request.options);
        assert_eq!(allowed.as_deref(), Some("approved"));
        // Another server's tool, or a command, is the user's to answer.
        let theirs: RequestPermissionRequest = serde_json::from_str(
            r#"{"sessionId":"s","toolCall":{"toolCallId":"x","title":"mcp.other.run",
                "rawInput":{"serverName":"other"}},
                "options":[{"optionId":"approved","name":"Allow","kind":"allow_once"}]}"#,
        )
        .unwrap();
        assert_eq!(
            chat.about_own_tools(&theirs.tool_call, &theirs.options),
            None
        );
        // A command titled as one of ours, as Codex titles a command with
        // its text, is still a command, and the user's to answer.
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"c",
                "title":"mcp__clackatoo__list_terminals && rm -rf ~",
                "kind":"execute","status":"pending","rawInput":{"command":["sh"]}}"#,
        );
        let posing: RequestPermissionRequest = serde_json::from_str(
            r#"{"sessionId":"s","toolCall":{"toolCallId":"c","kind":"execute"},
                "options":[{"optionId":"approved","name":"Allow","kind":"allow_once"}]}"#,
        )
        .unwrap();
        assert_eq!(
            chat.about_own_tools(&posing.tool_call, &posing.options),
            None
        );
    }

    #[test]
    fn an_answer_to_a_question_taken_back_lands_on_no_other() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        chat.ask_for_tool(1, "run `a` in a new terminal", None, Choices::Tool);
        chat.ask_for_tool(2, "run `b` in a new terminal", None, Choices::Tool);
        let shown = chat.node.question.clone().unwrap();
        // The call is cancelled as the user answers it.
        chat.withdraw(1);
        let stale = ChatAction::Answer {
            question: shown.id,
            option: Some("allow".into()),
        };
        assert_eq!(chat.act(stale.clone()), []);
        let next = chat.node.question.clone().unwrap();
        assert_eq!(next.title, "run `b` in a new terminal");
        // Answered once, a second press of the same answer does nothing.
        let once = ChatAction::Answer {
            question: next.id,
            option: Some("allow".into()),
        };
        assert_eq!(
            chat.act(once.clone()),
            [Action::Permit {
                call: 2,
                allow: true,
                pair: false,
            }]
        );
        assert_eq!(chat.act(once), []);
    }

    #[test]
    fn the_tools_setting_is_the_chats_own_and_never_the_agents() {
        let mut chat = chat();
        connect(&mut chat, "s");
        let set = |value: &str| ChatAction::Set {
            setting: TOOLS_SETTING.into(),
            value: value.into(),
        };
        assert_eq!(chat.act(set("allow")), [], "the agent never hears of it");
        assert_eq!(chat.tools(), AgentTools::Allow);
        let shown = (chat.node.settings.iter()).find(|setting| setting.id == TOOLS_SETTING);
        assert_eq!(shown.map(|setting| setting.value.as_str()), Some("allow"));
        assert_eq!(chat.act(set("ask")), []);
        assert_eq!(chat.tools(), AgentTools::Ask);
        assert_eq!(chat.act(set("everything")), []);
        assert_eq!(chat.tools(), AgentTools::Ask);
        // Kept when the agent's settings change, before the browser's.
        chat.settings_changed(Ok(claude_settings("sonnet")));
        let ids: Vec<&str> = (chat.node.settings.iter().rev().take(2))
            .map(|setting| setting.id.as_str())
            .collect();
        assert_eq!(ids, [BROWSER_SETTING, TOOLS_SETTING]);
    }

    #[test]
    fn the_browser_setting_is_the_chats_own_and_off_unless_asked_for() {
        let mut chat = chat();
        connect(&mut chat, "s");
        assert!(!chat.browser(), "off for a new chat");
        let set = |value: &str| ChatAction::Set {
            setting: BROWSER_SETTING.into(),
            value: value.into(),
        };
        // The agent never hears of it; the server gives it the tools.
        assert_eq!(chat.act(set("on")), [Action::Browser(true)]);
        assert!(chat.browser());
        assert_eq!(chat.act(set("on")), [], "already on");
        let shown = (chat.node.settings.iter()).find(|setting| setting.id == BROWSER_SETTING);
        assert_eq!(shown.map(|setting| setting.value.as_str()), Some("on"));

        // Letting it open pages is remembered, and asked about no more...
        chat.ask_for_tool(4, "open https://example.com", None, Choices::Browser);
        let question = chat.node.question.clone().unwrap();
        let ids: Vec<&str> = (question.choices.iter())
            .map(|choice| choice.id.as_str())
            .collect();
        assert_eq!(ids, ["browse", "reject"]);
        let answer = ChatAction::Answer {
            question: question.id,
            option: Some("browse".into()),
        };
        assert_eq!(
            chat.act(answer),
            [Action::Permit {
                call: 4,
                allow: true,
                pair: false
            }]
        );
        assert!(chat.browsing());
        assert_eq!(
            chat.tools(),
            AgentTools::Ask,
            "the Tools setting is untouched"
        );

        // ...until the browser goes off, which forgets it.
        assert_eq!(chat.act(set("off")), [Action::Browser(false)]);
        assert!(!chat.browser() && !chat.browsing());
        assert_eq!(chat.act(set("sometimes")), []);
    }

    #[test]
    fn too_many_questions_are_refused_and_long_text_is_shown_whole() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        let whole = "x".repeat(5000);
        assert!(chat.ask_for_tool(
            0,
            "type 5 KB of text into pane 2",
            Some(&whole),
            Choices::Tool
        ));
        let card = (chat.node.turns[0].items.iter()).find_map(|item| match item {
            Item::Tool(tool) => Some(tool.content.clone()),
            _ => None,
        });
        assert_eq!(
            card,
            Some(vec![ToolContent::Text(format!("```\n{whole}\n```"))])
        );
        for call in 1..MAX_QUESTIONS as u64 {
            assert!(chat.ask_for_tool(call, "run `x` in a new terminal", None, Choices::Tool));
        }
        assert!(!chat.ask_for_tool(99, "run `y` in a new terminal", None, Choices::Tool));
    }

    #[test]
    fn stopping_refuses_the_tools_questions_too() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        chat.ask_for_tool(3, "type `y` into pane 2", None, Choices::Tool);
        assert_eq!(
            chat.act(ChatAction::Stop),
            [
                Action::Permit {
                    call: 3,
                    allow: false,
                    pair: false,
                },
                Action::Cancel
            ]
        );
        assert_eq!(chat.node.question, None);
    }

    #[test]
    fn a_session_slow_to_open_is_asked_about_first_and_esc_keeps_waiting() {
        let mut chat = chat();
        chat.suggest_install("Install Claude Code's adapter for faster starts?");
        chat.slow_to_open("slow".to_owned());
        // Not hidden behind the suggestion, which stays after it.
        let question = chat.node.question.clone().unwrap();
        assert!(question.own);
        assert_eq!(
            (question.choices.iter())
                .map(|choice| choice.id.as_str())
                .collect::<Vec<_>>(),
            ["wait", "retry", "stop"]
        );
        // What Esc picks, the one refusing this once, keeps waiting, as
        // does no answer: neither gives up, nor starts anything again.
        let refusal = (question.choices.iter())
            .find(|choice| choice.kind == ChoiceKind::RejectOnce)
            .unwrap();
        assert_eq!(refusal.id, "wait");
        let answered = chat.act(ChatAction::Answer {
            question: question.id,
            option: None,
        });
        assert_eq!(answered, []);
        assert!(chat.is_opening());
        assert_eq!(chat.node.notice.as_deref(), Some("slow"));
        assert!(chat.offering_install(), "the suggestion is next");
    }

    #[test]
    fn a_message_waiting_for_a_session_let_go_survives_the_agent_starting_again() {
        let mut chat = chat();
        connect(&mut chat, "s");
        assert_eq!(chat.park().as_deref(), Some("s"));
        send(&mut chat, "later");
        // Asks the new process for it, and the message waits for that.
        assert!(chat.agent_restarted());
        assert!(chat.is_opening());
        assert_eq!(outcome(&chat.node.turns[0]), None);
        assert_eq!(connect(&mut chat, ""), [Action::Prompt("later".into())]);
        // Let go with nothing waiting, it's opened with the next message.
        chat.answered(Ok(StopReason::EndTurn));
        chat.park();
        assert!(!chat.agent_restarted());
        assert_eq!(send(&mut chat, "again"), [Action::Connect]);
        // An open session is gone, and a waiting one is asked for again.
        let mut chat = super::tests::chat();
        assert!(chat.agent_restarted());
        connect(&mut chat, "s");
        assert!(!chat.agent_restarted());
        assert!(!chat.is_connected());
    }

    #[test]
    fn a_session_slow_to_open_is_said_once_and_goes_when_it_opens() {
        let mut chat = chat();
        chat.slow_to_open("slow".to_owned());
        chat.slow_to_open("slow".to_owned());
        assert_eq!(chat.node.notice.as_deref(), Some("slow"));
        assert_eq!(chat.asking.len(), 1, "asked once");
        let question = chat.node.question.clone().unwrap();
        let retry = chat.act(ChatAction::Answer {
            question: question.id,
            option: Some("retry".to_owned()),
        });
        assert_eq!(retry, [Action::Retry]);
        assert!(chat.node.notice.as_deref().unwrap().ends_with("again…"));
        assert_eq!(chat.node.question, None);
        connect(&mut chat, "s");
        assert_eq!(
            (chat.node.notice.clone(), chat.node.question.clone()),
            (None, None)
        );
        // Open, it isn't slow.
        chat.slow_to_open("slow".to_owned());
        assert_eq!(chat.node.notice, None);

        // A notice said since isn't the chat's to take away.
        let mut chat = super::tests::chat();
        chat.slow_to_open("slow".to_owned());
        chat.notify("something else".to_owned());
        connect(&mut chat, "s");
        assert_eq!(chat.node.notice.as_deref(), Some("something else"));
        assert_eq!(chat.node.question, None);
    }

    #[test]
    fn stopping_cancels_turns_down_the_question_or_takes_back_the_message() {
        let mut chat = chat();
        // Not yet sent: taken back, and the session isn't waited for.
        send(&mut chat, "hello");
        assert_eq!(chat.act(ChatAction::Stop), [Action::Abandon]);
        assert_eq!(outcome(&chat.node.turns[0]), Some(&Outcome::Stopped));
        assert_eq!(chat.node.state, ChatState::Ready);
        // Sent, with a question waiting: none for it, and a cancel.
        connect(&mut chat, "s");
        send(&mut chat, "go");
        let tool: ToolCallUpdate = serde_json::from_str(r#"{"toolCallId":"t"}"#).unwrap();
        chat.ask(Value::from(3), &tool, &[]);
        assert_eq!(
            chat.act(ChatAction::Stop),
            [
                Action::Answer {
                    request: Value::from(3),
                    option: None,
                },
                Action::Cancel,
            ]
        );
        chat.answered(Ok(StopReason::Cancelled));
        assert_eq!(outcome(&chat.node.turns[1]), Some(&Outcome::Stopped));
    }

    #[test]
    fn the_agent_stopping_ends_the_answer_or_says_so() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"t","title":"Run","status":"in_progress"}"#,
        );
        chat.disconnected(None);
        let turn = &chat.node.turns[0];
        assert_eq!(
            outcome(turn),
            Some(&Outcome::Failed("the agent stopped".into()))
        );
        assert!(matches!(&turn.items[0], Item::Tool(tool) if tool.status == ToolStatus::Failed));
        // Idle, it says so, and the next message opens the session again.
        connect(&mut chat, "");
        chat.disconnected(None);
        assert!(chat.node.notice.is_some());
        assert_eq!(send(&mut chat, "hi"), [Action::Connect]);
        assert_eq!(chat.node.notice, None);
    }

    #[test]
    fn what_the_agent_writes_keeps_its_lines_but_no_controls() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "hi");
        words(&mut chat, "a\u{1b}]2;x\u{7}b\n\tc");
        assert_eq!(
            chat.node.turns[0].items,
            [Item::Words("a]2;xb\n\tc".into())]
        );
    }

    #[test]
    fn a_chat_handed_over_keeps_its_turns_and_cuts_off_an_answer() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "hi");
        words(&mut chat, "hello");
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let resumed = HostedChat::resume(chat.hand_over(), &agent);
        assert_eq!(resumed.session(), Some("s"));
        let turn = &resumed.node.turns[0];
        assert_eq!(turn.items, [Item::Words("hello".into())]);
        assert_eq!(
            outcome(turn),
            Some(&Outcome::Failed("cut off by an upgrade".into()))
        );
        assert_eq!(resumed.node.state, ChatState::Ready);
    }

    #[test]
    fn a_chat_without_claude_codes_task_tool_stays_without_it() {
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let size = Size::new(40, 12).unwrap();
        let tools = AgentTools::Ask;
        let mut chat =
            HostedChat::new(TerminalId(1), size, "/".into(), &agent, tools, false, false);
        assert!(!chat.task());
        // It's never shown, and never sent to the agent as a setting.
        let actions = connect(&mut chat, "s");
        assert!(
            !(chat.node.settings.iter()).any(|setting| setting.id == TASK_SETTING),
            "{:?}",
            chat.node.settings
        );
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, Action::Set { .. })),
            "{actions:?}"
        );
        // Nor can a window change it once the session is open.
        chat.act(ChatAction::Set {
            setting: TASK_SETTING.into(),
            value: "on".into(),
        });
        assert!(!chat.task());
        // It lasts through an upgrade, so the session opens again without it.
        let resumed = HostedChat::resume(chat.hand_over(), &agent);
        assert!(!resumed.task());
        // A chat from before it could be taken away keeps it.
        let mut handed = resumed.hand_over();
        handed.chosen.retain(|(id, _)| id != TASK_SETTING);
        assert!(HostedChat::resume(handed, &agent).task());
        assert!(chat_with("claude").task());
    }

    #[test]
    fn a_chat_that_moved_keeps_where_it_moved_through_an_upgrade_and_can_go_back() {
        let config = crate::agents::AgentConfig::default();
        let (claude, codex) = (
            config.find("claude").unwrap(),
            config.find("codex").unwrap(),
        );
        let mut chat = chat();
        chat.connected(Opened {
            session: "c".into(),
            settings: Some(claude_settings("sonnet")),
        });
        send(&mut chat, "hi");
        words(&mut chat, "hello");
        chat.answered(Ok(StopReason::EndTurn));
        assert_eq!(
            chat.switch(&codex, None, None, 5_000),
            Some(("claude".to_owned(), Some("c".to_owned())))
        );
        connect(&mut chat, "x");
        // The first message to Codex carries the first turn.
        assert_eq!(
            send(&mut chat, "next"),
            [Action::Carry {
                text: "next".into(),
                from: 0,
                to: 2
            }]
        );
        chat.answered(Ok(StopReason::EndTurn));

        let mut resumed = HostedChat::resume(chat.hand_over(), &codex);
        assert_eq!((resumed.agent(), resumed.session()), ("codex", Some("x")));
        assert!(resumed.node.turns[1].switch.is_some());
        assert_eq!(resumed.parked()[0].session.as_deref(), Some("c"));
        // Already told: the next goes as it is.
        connect(&mut resumed, "");
        assert_eq!(send(&mut resumed, "more"), [Action::Prompt("more".into())]);
        resumed.answered(Ok(StopReason::EndTurn));
        // Back to Claude Code's session, with the model it had.
        assert_eq!(
            resumed.switch(&claude, None, None, 5_000),
            Some(("codex".to_owned(), Some("x".to_owned())))
        );
        assert_eq!(resumed.session(), Some("c"));
        let model = resumed
            .setting_of(SettingKind::Model)
            .map(|setting| &setting.value);
        assert_eq!(model.map(String::as_str), Some("sonnet"));
        assert!(
            resumed
                .node
                .turns
                .last()
                .unwrap()
                .switch
                .as_ref()
                .unwrap()
                .resumed
        );
    }

    /// Codex's settings, as its adapter gives them, in short: its modes,
    /// starting at Auto review, and its models.
    fn codex_settings() -> Vec<SessionConfigOption> {
        serde_json::from_str(
            r#"[{"id":"mode","name":"Mode","category":"mode","type":"select","currentValue":"agent",
                 "options":[{"value":"read-only","name":"Read-only"},{"value":"agent","name":"Auto review"}]},
                {"id":"model","name":"Model","category":"model","type":"select","currentValue":"gpt-6-astra",
                 "options":[{"value":"gpt-6-astra","name":"6 Astra"},{"value":"gpt-6-luna","name":"6 Luna"}]}]"#,
        )
        .unwrap()
    }

    /// A chat with Claude Code that has answered one message, `hi`, and
    /// then moved to Codex, whose session has opened.
    fn moved_to_codex(chat: &mut HostedChat) {
        let codex = crate::agents::AgentConfig::default().find("codex").unwrap();
        chat.connected(Opened {
            session: "c".into(),
            settings: Some(claude_settings("haiku")),
        });
        send(chat, "hi");
        words(chat, "hello");
        chat.answered(Ok(StopReason::EndTurn));
        chat.switch(&codex, None, None, handoff::BUDGET);
        chat.connected(Opened {
            session: "x".into(),
            settings: Some(codex_settings()),
        });
    }

    #[test]
    fn a_digest_that_failed_to_go_goes_again_with_the_next_message() {
        let mut chat = chat();
        moved_to_codex(&mut chat);
        let carry = |text: &str, to| Action::Carry {
            text: text.into(),
            from: 0,
            to,
        };
        assert_eq!(send(&mut chat, "one"), [carry("one", 2)]);
        chat.answered(Err("overloaded".into()));
        // Not heard: it goes again, with the turn that failed.
        assert_eq!(send(&mut chat, "two"), [carry("two", 3)]);
        chat.answered(Ok(StopReason::EndTurn));
        assert_eq!(send(&mut chat, "three"), [Action::Prompt("three".into())]);
    }

    #[test]
    fn another_agents_plan_is_told_once_then_left_to_the_user() {
        let mut chat = chat();
        connect(&mut chat, "c");
        send(&mut chat, "plan it");
        update(
            &mut chat,
            r#"{"sessionUpdate":"plan","entries":[{"content":"Test it","priority":"high","status":"pending"}]}"#,
        );
        chat.answered(Ok(StopReason::EndTurn));
        let codex = crate::agents::AgentConfig::default().find("codex").unwrap();
        chat.switch(&codex, None, None, handoff::BUDGET);
        connect(&mut chat, "x");
        send(&mut chat, "go on");
        let digest = chat.digest(0, 2, &[]).unwrap();
        assert!(
            digest.contains("(ask the user before continuing it)\nClaude Code's, as it last gave it:\n- [ ] Test it"),
            "{digest}"
        );
        chat.answered(Ok(StopReason::EndTurn));
        assert!(chat.plan.is_empty());
    }

    #[test]
    fn a_digest_keeps_to_a_quarter_of_the_agents_context_and_at_most_the_most() {
        let mut chat = chat();
        connect(&mut chat, "c");
        for at in 0..400 {
            send(&mut chat, &format!("ask {at}"));
            words(&mut chat, &"an answer of some length ".repeat(40));
            chat.answered(Ok(StopReason::EndTurn));
        }
        let codex = crate::agents::AgentConfig::default().find("codex").unwrap();
        chat.switch(&codex, None, None, 1_000_000);
        connect(&mut chat, "x");
        let to = chat.node.turns.len();
        let most = chat.digest(0, to, &[]).unwrap();
        assert!(
            handoff::tokens(&most) <= handoff::MOST,
            "{}",
            handoff::tokens(&most)
        );
        assert!(handoff::tokens(&most) > handoff::MOST / 2);
        chat.set_usage(Some(Usage {
            used: 0,
            size: 40_000,
            five_hour: None,
            weekly: None,
        }));
        let quarter = chat.digest(0, to, &[]).unwrap();
        assert!(
            handoff::tokens(&quarter) <= 10_000,
            "{}",
            handoff::tokens(&quarter)
        );
    }

    #[test]
    fn a_chat_an_agent_started_moves_to_the_new_agents_safest_mode() {
        let mut chat = chat();
        chat.started_by(TerminalId(7), TerminalId(7), 1, Some("auto".into()));
        moved_to_codex(&mut chat);
        let mode = chat.setting_of(SettingKind::Mode).unwrap();
        assert_eq!(mode.value, "read-only");
        let switch = chat.node.turns[1].switch.as_ref().unwrap();
        assert_eq!(
            switch.notes,
            [
                "Mode Auto doesn't carry over: Codex uses Read-only, its safest, as an agent started this chat"
            ]
        );
        // The user's own chat keeps Codex's own.
        let mut chat = super::tests::chat();
        moved_to_codex(&mut chat);
        assert_eq!(chat.setting_of(SettingKind::Mode).unwrap().value, "agent");
    }

    #[test]
    fn a_parents_read_only_mode_holds_as_limits_and_a_chat_with_no_mode_to_start_in_starts_safest()
    {
        use crate::profiles::Limits;
        let codex_in = |mode: &str| {
            let mut settings = codex_settings();
            settings[0] = serde_json::from_str(&format!(
                r#"{{"id":"mode","name":"Mode","category":"mode","type":"select","currentValue":"{mode}",
                     "options":[{{"value":"read-only","name":"Read-only"}},{{"value":"agent","name":"Auto review"}}]}}"#
            ))
            .unwrap();
            settings
        };
        let mut parent = chat_with("codex");
        parent.connected(Opened {
            session: "p".into(),
            settings: Some(codex_in("read-only")),
        });
        assert_eq!(parent.mode_limits(), Limits::ReadOnly);
        let mut parent = chat_with("codex");
        parent.connected(Opened {
            session: "p".into(),
            settings: Some(codex_in("agent")),
        });
        assert_eq!(parent.mode_limits(), Limits::None);
        // Claude Code's planning changes nothing either.
        let mut planning = chat();
        planning.connected(Opened {
            session: "p".into(),
            settings: Some(
                serde_json::from_str(
                    r#"[{"id":"mode","name":"Mode","category":"mode","type":"select","currentValue":"plan",
                         "options":[{"value":"default","name":"Manual"},{"value":"plan","name":"Plan"}]}]"#,
                )
                .unwrap(),
            ),
        });
        assert_eq!(planning.mode_limits(), Limits::ReadOnly);

        // Started by an agent, with no mode of its parent's to start in,
        // as with another agent: its agent's safest, never its own choice.
        let mut kid = chat_with("codex");
        kid.started_by(TerminalId(7), TerminalId(7), 1, None);
        let actions = kid.connected(Opened {
            session: "k".into(),
            settings: Some(codex_settings()),
        });
        let read_only = Action::Set {
            setting: "mode".into(),
            value: "read-only".into(),
        };
        assert!(actions.contains(&read_only), "{actions:?}");
        // One that may run no commands opens every new session read-only,
        // as Codex runs sandboxed commands without asking otherwise.
        let mut kid = chat_with("codex");
        kid.started_by(TerminalId(7), TerminalId(7), 1, Some("agent".into()));
        kid.start_as("x", Limits::NoCommands, None, Wanted::default());
        kid.wanted = None;
        let actions = kid.connected(Opened {
            session: "k".into(),
            settings: Some(codex_settings()),
        });
        assert!(actions.contains(&read_only), "{actions:?}");
    }

    #[test]
    fn a_session_let_go_is_opened_again_only_once_the_agent_has_ended_it() {
        let mut chat = chat();
        connect(&mut chat, "s");
        assert_eq!(chat.park().as_deref(), Some("s"));
        // A message meanwhile waits: opening it now could cross the end.
        assert_eq!(send(&mut chat, "more"), []);
        assert_eq!(chat.session_closed(), [Action::Connect]);
        assert_eq!(chat.session_closed(), []);
        // With none meanwhile, the next message opens it.
        let mut chat = super::tests::chat();
        connect(&mut chat, "s");
        chat.park();
        assert_eq!(chat.session_closed(), []);
        assert_eq!(send(&mut chat, "later"), [Action::Connect]);
        // An agent that can't end sessions has nothing to wait for.
        let mut chat = super::tests::chat();
        connect(&mut chat, "s");
        chat.park();
        chat.parked_without_closing();
        assert_eq!(send(&mut chat, "now"), [Action::Connect]);
    }

    #[test]
    fn a_session_that_couldnt_be_gone_back_to_isnt_said_to_be() {
        let mut chat = chat();
        moved_to_codex(&mut chat);
        let claude = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        chat.switch(&claude, None, None, handoff::BUDGET);
        assert!(chat.node.turns[2].switch.as_ref().unwrap().resumed);
        chat.session_lost();
        let switch = chat.node.turns[2].switch.as_ref().unwrap();
        assert!(!switch.resumed);
        assert_eq!(chat.session(), None);
        assert!(matches!(
            chat.journal.patches.back(),
            Some(ChatPatch::Turn { index: 2, .. })
        ));
    }

    /// A message from chat `from`, `hops` along a chain.
    fn message(from: u32, text: &str, hops: u8) -> Message {
        Message {
            from: Sender {
                chat: TerminalId(from),
                agent: "Codex".into(),
                title: "fixes".into(),
            },
            text: text.into(),
            hops,
            cause: None,
        }
    }

    #[test]
    fn a_message_from_another_agent_waits_for_the_turn_then_goes_marked() {
        let mut chat = chat();
        connect(&mut chat, "s");
        // Idle, it goes at once, as a turn marked as from the sender, the
        // agent told who sent it.
        let actions = chat.deliver(message(7, "hi", 1)).unwrap();
        let [Action::Prompt(prompt)] = actions.as_slice() else {
            panic!("{actions:?}");
        };
        assert!(
            prompt
                .text
                .starts_with("[Message from chat 7 (Codex, \"fixes\"): another agent"),
            "{prompt:?}"
        );
        assert!(
            prompt
                .text
                .contains(" from chat 7 ---\nhi\n--- end of message "),
            "{prompt:?}"
        );
        let turn = &chat.node.turns[0];
        assert_eq!(turn.prompt, "hi");
        assert_eq!(turn.from.as_ref().unwrap().chat, TerminalId(7));
        assert_eq!(chat.relations().hops, 1);
        // Answering, the next waits, up to the most kept.
        for n in 0..MAX_INBOX {
            assert_eq!(chat.deliver(message(7, &format!("{n}"), 1)), Ok(Vec::new()));
        }
        assert!(chat.deliver(message(7, "one too many", 1)).is_err());
        assert_eq!(chat.deliver_next(), [], "not while answering");
        chat.answered(Ok(StopReason::EndTurn));
        let next = chat.deliver_next();
        assert!(
            matches!(next.as_slice(), [Action::Prompt(prompt)] if prompt.text.contains(" ---\n0\n--- end"))
        );
        assert_eq!(chat.relations().inbox.len(), MAX_INBOX - 1);
        // The user's own message starts the chain again.
        chat.answered(Ok(StopReason::EndTurn));
        chat.relations.inbox.clear();
        send(&mut chat, "mine");
        assert_eq!(chat.relations().hops, 0);
        assert_eq!(chat.node.turns.last().unwrap().from, None);
    }

    #[test]
    fn always_allowing_a_message_is_for_that_chat_and_never_the_tools_setting() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "work");
        assert!(chat.ask_for_tool(4, "send `hi` to chat 2", None, Choices::Talk));
        let question = chat.node.question.clone().unwrap();
        let names: Vec<&str> = (question.choices.iter())
            .map(|choice| choice.name.as_str())
            .collect();
        assert_eq!(names, ["Allow", "Always allow for that chat", "Reject"]);
        assert_eq!(
            answer(&mut chat, Some("pair".into())),
            [Action::Permit {
                call: 4,
                allow: true,
                pair: true,
            }]
        );
        assert_eq!(chat.tools(), AgentTools::Ask, "the setting is as it was");
    }

    #[test]
    fn stopping_drops_the_messages_waiting_and_says_so() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "work");
        chat.deliver(message(7, "later", 1)).unwrap();
        assert_eq!(chat.act(ChatAction::Stop), [Action::Cancel]);
        assert!(chat.relations().inbox.is_empty());
        assert_eq!(
            chat.node.notice.as_deref(),
            Some("Stopped: 1 message from other agents waiting their turn weren't sent.")
        );
    }

    #[test]
    fn where_a_chat_stands_among_the_others_survives_an_upgrade() {
        let mut chat = chat();
        connect(&mut chat, "s");
        chat.started_by(TerminalId(3), TerminalId(2), 1, None);
        chat.deliver(message(3, "do it", 2)).unwrap();
        chat.answered(Ok(StopReason::EndTurn));
        send(&mut chat, "and you");
        chat.deliver(message(5, "waiting", 3)).unwrap();
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let resumed = HostedChat::resume(chat.hand_over(), &agent);
        assert_eq!(resumed.relations().parent, Some(TerminalId(3)));
        assert_eq!(resumed.relations().depth, 1);
        assert_eq!(resumed.relations().inbox, [message(5, "waiting", 3)]);
        let senders: Vec<Option<TerminalId>> = (resumed.node.turns.iter())
            .map(|turn| turn.from.as_ref().map(|from| from.chat))
            .collect();
        assert_eq!(senders, [Some(TerminalId(3)), None]);
        // It isn't one of the values the agent is asked for.
    }

    fn set(setting: &str, value: &str) -> Action {
        Action::Set {
            setting: setting.into(),
            value: value.into(),
        }
    }

    #[test]
    fn a_new_chat_asks_for_auto_mode_and_choices_outlast_the_agent() {
        let mut chat = chat();
        let opened = |model: &str| Opened {
            session: "s".into(),
            settings: Some(claude_settings(model)),
        };
        assert_eq!(chat.connected(opened("haiku")), [set("mode", "auto")]);
        let modes: Vec<(SettingKind, &str)> = (chat.node.settings.iter())
            .map(|setting| (setting.kind, setting.value.as_str()))
            .collect();
        // The chat's own setting for clackatoo's tools comes after the
        // agent's.
        assert_eq!(
            modes,
            [
                (SettingKind::Mode, "auto"),
                (SettingKind::Model, "haiku"),
                (SettingKind::Tools, "ask"),
                (SettingKind::Browser, "off"),
            ]
        );

        // A choice goes to the agent, and the view shows it at once.
        assert_eq!(
            chat.act(ChatAction::Set {
                setting: "model".into(),
                value: "sonnet".into(),
            }),
            [set("model", "sonnet")]
        );
        // A value it doesn't offer goes nowhere.
        assert_eq!(
            chat.act(ChatAction::Set {
                setting: "model".into(),
                value: "gpt".into(),
            }),
            []
        );

        // The agent stops, and comes back having forgotten: the choices go
        // again, but Auto only as the user had it.
        chat.disconnected(None);
        assert_eq!(
            chat.connected(opened("haiku")),
            [set("mode", "auto"), set("model", "sonnet")]
        );
    }

    #[test]
    fn a_chat_without_auto_asks_for_nothing() {
        let mut chat = chat();
        let settings: Vec<SessionConfigOption> = serde_json::from_str(
            r#"[{"id":"mode","name":"Mode","category":"mode","type":"select","currentValue":"ask",
                 "options":[{"value":"ask","name":"Ask"}]}]"#,
        )
        .unwrap();
        let opened = Opened {
            session: "s".into(),
            settings: Some(settings),
        };
        assert_eq!(chat.connected(opened), []);
    }

    #[test]
    fn usage_and_settings_follow_the_agent() {
        let mut chat = chat();
        connect(&mut chat, "s");
        update(
            &mut chat,
            r#"{"sessionUpdate":"usage_update","used":22118,"size":1000000}"#,
        );
        let usage = Usage {
            used: 22118,
            size: 1_000_000,
            five_hour: None,
            weekly: None,
        };
        assert_eq!(chat.node.usage, Some(usage));
        // Its account's limits, as the daemon heard them, go with it, and
        // with the next it says.
        let five_hour = Some(clackatoo_protocol::Limit {
            used: 42,
            resets: None,
        });
        chat.set_limits(Limits {
            five_hour,
            weekly: None,
        });
        assert_eq!(chat.node.usage, Some(Usage { five_hour, ..usage }));
        assert!(matches!(
            chat.journal.patches.back(),
            Some(ChatPatch::Usage(Some(Usage {
                five_hour: Some(_),
                ..
            })))
        ));
        // Nonsense counts are cut to sense.
        update(
            &mut chat,
            r#"{"sessionUpdate":"usage_update","used":18446744073709551615,"size":0}"#,
        );
        assert_eq!(
            chat.node.usage,
            Some(Usage {
                used: limits::MOST_TOKENS,
                size: 0,
                five_hour,
                weekly: None,
            })
        );
        chat.settings_changed(Ok(claude_settings("sonnet")));
        update(
            &mut chat,
            r#"{"sessionUpdate":"current_mode_update","currentModeId":"auto"}"#,
        );
        assert_eq!(chat.node.settings[0].value, "auto");
        assert_eq!(chat.node.settings[1].value, "sonnet");
    }

    #[test]
    fn a_chat_is_named_after_its_agent_and_starts_in_the_agents_own_mode() {
        let chat = chat_with("codex");
        assert_eq!(
            (chat.title.as_str(), chat.node.agent.as_str()),
            ("Codex", "Codex")
        );
        assert_eq!(chat.agent(), "codex");
        assert_eq!(chat.starting_mode, None);
        assert_eq!(chat_with("claude").starting_mode.as_deref(), Some("auto"));
    }

    /// Has `chat`'s agent stream `data` as the output of tool `e`.
    fn output(chat: &mut HostedChat, data: &str) {
        let data = serde_json::to_string(data).unwrap();
        update(
            chat,
            &format!(
                r#"{{"sessionUpdate":"tool_call_update","toolCallId":"e",
                    "_meta":{{"terminal_output_delta":{{"data":{data},"terminal_id":"e"}}}}}}"#
            ),
        );
    }

    /// Starts tool `e`, a command, as Codex's adapter does.
    fn start_command(chat: &mut HostedChat) {
        update(
            chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"e","title":"cat x","kind":"execute",
                "status":"in_progress","content":[{"type":"terminal","terminalId":"e"}]}"#,
        );
    }

    #[test]
    fn streamed_output_goes_as_appends_and_is_cut_to_its_end() {
        let mut chat = chat_with("codex");
        connect(&mut chat, "s");
        send(&mut chat, "go");
        start_command(&mut chat);
        let shown = |chat: &HostedChat| match &chat.node.turns[0].items[0] {
            Item::Tool(tool) => tool.content.clone(),
            other => panic!("{other:?}"),
        };
        // Nothing shown of a terminal the daemon never made.
        assert_eq!(shown(&chat), []);
        let before = chat.journal.end();
        output(&mut chat, "a ``` b\n");
        assert_eq!(shown(&chat), [ToolContent::Output("a ``` b\n".into())]);
        assert_eq!(
            chat.journal.patches.back(),
            Some(&ChatPatch::Output {
                turn: 0,
                item: 0,
                text: "a ``` b\n".into(),
            })
        );
        assert_eq!(chat.journal.end(), before + 1, "the piece alone");
        let line = "x".repeat(99) + "\n";
        for _ in 0..1000 {
            output(&mut chat, &line);
        }
        let content = shown(&chat);
        let [ToolContent::Output(text)] = content.as_slice() else {
            panic!()
        };
        assert!(text.len() <= 2 * TOOL_TEXT, "{}", text.len());
        assert!(
            text.starts_with("…\nxxx") && text.ends_with("x\n"),
            "{text}"
        );
    }

    #[test]
    fn a_flood_of_output_keeps_the_journal_bounded_and_a_late_viewer_gets_it_once() {
        let mut chat = chat_with("codex");
        connect(&mut chat, "s");
        let mut late = Viewer::new();
        late.catch_up(&chat);
        let mut keeping_up = Viewer::new();
        keeping_up.catch_up(&chat);
        send(&mut chat, "build");
        start_command(&mut chat);
        let line = "y".repeat(199) + "\n";
        for at in 0..10_000 {
            output(&mut chat, &line);
            if at % 1000 == 0 {
                keeping_up.catch_up(&chat);
            }
        }
        assert!(
            chat.journal.bytes <= JOURNAL_BYTES,
            "{}",
            chat.journal.bytes
        );
        assert!(chat.journal.patches.len() > 1);
        // Behind by the whole flood: one copy of the output, not one for
        // each piece, however it comes.
        let update = late.catch_up(&chat).unwrap();
        let size = postcard::to_allocvec(&update).unwrap().len();
        assert!(size < 4 * TOOL_TEXT, "{size}");
        keeping_up.catch_up(&chat);
        for viewer in [&late, &keeping_up] {
            assert_eq!(viewer.view.as_ref(), Some(&chat.view()));
        }
    }

    #[test]
    fn a_later_value_of_an_item_takes_the_place_of_the_first_in_an_update() {
        let mut chat = chat_with("codex");
        connect(&mut chat, "s");
        send(&mut chat, "go");
        let mut viewer = Viewer::new();
        viewer.catch_up(&chat);
        start_command(&mut chat);
        output(&mut chat, "one\n");
        // Another tool after it, then the first again, finished.
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"f","title":"ls","kind":"execute",
                "status":"in_progress"}"#,
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"e","status":"completed"}"#,
        );
        output(&mut chat, "two\n");
        let Some(ViewUpdate::Patch(patches)) = viewer.catch_up(&chat) else {
            panic!()
        };
        let items: Vec<(u32, bool)> = (patches.iter())
            .filter_map(|patch| match patch {
                Patch::Chat(ChatPatch::Item { item, .. }) => Some((*item, false)),
                Patch::Chat(ChatPatch::Output { item, .. }) => Some((*item, true)),
                _ => None,
            })
            .collect();
        // The first tool once, where it was first added, with the output
        // it had by then; the second; then the output since.
        assert_eq!(items, [(0, false), (1, false), (0, true)]);
        assert_eq!(viewer.view.as_ref(), Some(&chat.view()));
    }

    #[test]
    fn a_question_says_what_the_tool_said_it_does() {
        let mut chat = chat_with("codex");
        connect(&mut chat, "s");
        send(&mut chat, "go");
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"t","title":"rm -r build","kind":"execute",
                "status":"pending"}"#,
        );
        let request: RequestPermissionRequest = serde_json::from_str(
            r#"{"sessionId":"s","toolCall":{"toolCallId":"t","title":"Run command"},
                "options":[{"optionId":"a","name":"Yes","kind":"allow_once"}]}"#,
        )
        .unwrap();
        chat.ask(Value::from(1), &request.tool_call, &request.options);
        assert_eq!(chat.node.question.as_ref().unwrap().title, "rm -r build");
        answer(&mut chat, Some("a".into()));
        // One it hasn't said anything of goes by the question's.
        let request: RequestPermissionRequest = serde_json::from_str(
            r#"{"sessionId":"s","toolCall":{"toolCallId":"u","title":"Write a"},"options":[]}"#,
        )
        .unwrap();
        chat.ask(Value::from(2), &request.tool_call, &request.options);
        assert_eq!(chat.node.question.as_ref().unwrap().title, "Write a");
    }

    #[test]
    fn an_install_offer_holds_the_message_until_its_answered() {
        let mut chat = chat_with("codex");
        chat.offer_install("Codex isn't installed. Install it?");
        assert!(chat.offering_install());
        assert_eq!(send(&mut chat, "hi"), []);
        assert_eq!(answer(&mut chat, Some(INSTALL.into())), [Action::Install]);
        assert_eq!(chat.node.question, None);
        assert_eq!(chat.node.notice.as_deref(), Some("Installing Codex…"));
        assert_eq!(chat.node.state, ChatState::Starting);
        // Installed, its session opens and the message goes.
        chat.installed();
        assert_eq!(chat.node.notice, None);
        assert_eq!(connect(&mut chat, "s"), [Action::Prompt("hi".into())]);

        // No: the message goes nowhere, and the next asks again.
        let mut chat = chat_with("codex");
        chat.offer_install("Install?");
        send(&mut chat, "hi");
        assert_eq!(answer(&mut chat, Some("later".into())), []);
        assert!(
            matches!(&chat.node.state, ChatState::Failed(why) if why.contains("isn't installed"))
        );
        assert!(matches!(
            outcome(&chat.node.turns[0]),
            Some(Outcome::Failed(_))
        ));
        assert_eq!(send(&mut chat, "again"), [Action::Connect]);

        // Stopped while it's asked: the message is taken back.
        let mut chat = chat_with("codex");
        chat.offer_install("Install?");
        send(&mut chat, "hi");
        assert_eq!(chat.act(ChatAction::Stop), []);
        assert_eq!(outcome(&chat.node.turns[0]), Some(&Outcome::Stopped));
        assert_eq!(
            (chat.node.question.as_ref(), chat.offering_install()),
            (None, false)
        );
    }

    #[test]
    fn a_long_title_is_cut_short() {
        let mut chat = chat_with("codex");
        let title = "é".repeat(100);
        update(
            &mut chat,
            &format!(r#"{{"sessionUpdate":"session_info_update","title":"{title}"}}"#),
        );
        assert_eq!(chat.title, "é".repeat(TITLE) + "…");
    }

    #[test]
    fn an_agent_that_never_started_says_why() {
        let mut chat = chat_with("codex");
        send(&mut chat, "hi");
        chat.disconnected(Some("npm ERR! nope"));
        assert_eq!(
            chat.node.state,
            ChatState::Failed("it stopped, saying: npm ERR! nope".into())
        );
        assert_eq!(
            outcome(&chat.node.turns[0]),
            Some(&Outcome::Failed(
                "Codex couldn't start: it stopped, saying: npm ERR! nope".into()
            ))
        );
    }

    /// A piece of the conversation told again as the session loads.
    fn replay(chat: &mut HostedChat, json: &str) {
        let json = format!(r#"{{"sessionId":"past","update":{json}}}"#);
        let notification: SessionNotification = serde_json::from_str(&json).unwrap();
        chat.replay(notification.update);
    }

    /// The user's message `text`, told again.
    fn replay_user(chat: &mut HostedChat, text: &str) {
        let json = serde_json::json!({ "sessionUpdate": "user_message_chunk",
                                       "content": { "type": "text", "text": text } });
        replay(chat, &json.to_string());
    }

    /// A chat going on with a session its agent kept, its load asked for.
    fn resuming() -> HostedChat {
        let mut chat = chat();
        chat.go_on_with(clackatoo_protocol::ResumeSession {
            session: "past".to_owned(),
            title: String::new(),
        });
        assert!(chat.loading());
        chat
    }

    /// The messages of `chat`'s turns, in order.
    fn prompts(chat: &HostedChat) -> Vec<&str> {
        (chat.node.turns.iter())
            .map(|turn| turn.prompt.as_str())
            .collect()
    }

    #[test]
    fn messages_sent_while_a_session_loads_go_together_after_the_turns_told_again() {
        let mut chat = resuming();
        replay_user(&mut chat, "before");
        replay_user(&mut chat, "and its second block");
        // While the turn told again is open, as most of a load is.
        assert!(send(&mut chat, "and now?").is_empty(), "held");
        assert!(send(&mut chat, "also this").is_empty(), "joined, not lost");
        assert!(chat.node.notice.is_some(), "says it waits");
        replay(
            &mut chat,
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}"#,
        );
        chat.opened_again(true);
        let actions = connect(&mut chat, "past");
        let prompted: Vec<&Action> = (actions.iter())
            .filter(|action| matches!(action, Action::Prompt(_)))
            .collect();
        assert!(
            matches!(prompted[..], [Action::Prompt(text)] if text.text == "and now?\n\nalso this"),
            "{actions:?}"
        );
        assert_eq!(
            prompts(&chat),
            ["before\n\nand its second block", "and now?\n\nalso this"]
        );
        assert_eq!(outcome(&chat.node.turns[0]), Some(&Outcome::Done));
        assert_eq!(
            chat.known, 2,
            "nothing carried: the session saw the turn told again"
        );
    }

    #[test]
    fn a_load_refused_holds_no_message_and_loads_again_with_the_next() {
        let mut chat = resuming();
        send(&mut chat, "first");
        chat.failed_to_connect("no one's logged in");
        // The message held shows, failed, and the load is still to come.
        assert_eq!(prompts(&chat), ["first"]);
        assert!(matches!(
            outcome(&chat.node.turns[0]),
            Some(Outcome::Failed(_))
        ));
        assert!(chat.loading());
        // The next message asks for it again.
        assert_eq!(send(&mut chat, "second"), [Action::Connect]);
        assert_eq!(prompts(&chat), ["first"], "held for after the load");
    }

    #[test]
    fn stopping_a_load_shows_the_message_held_and_passes_over_the_rest_until_answered() {
        let mut chat = resuming();
        replay_user(&mut chat, "before");
        assert!(chat.answering(), "the turn told again is open");
        send(&mut chat, "held");
        let actions = chat.act(ChatAction::Stop);
        assert!(actions.contains(&Action::Abandon), "{actions:?}");
        assert!(!chat.loading() && chat.skipping());
        assert_eq!(prompts(&chat), ["before", "held"]);
        assert_eq!(outcome(&chat.node.turns[1]), Some(&Outcome::Stopped));
        // Its session opening again doesn't end the passing over: only the
        // agent answering the load it stopped waiting for does.
        chat.opened_again(false);
        assert!(chat.skipping());
        assert!(!chat.load_answered(), "it wasn't waiting to ask");
        assert!(!chat.skipping());
    }

    #[test]
    fn a_chat_waits_to_ask_for_its_session_until_a_load_it_left_is_answered() {
        let mut chat = resuming();
        replay_user(&mut chat, "before");
        chat.act(ChatAction::Stop);
        assert_eq!(send(&mut chat, "now"), [Action::Connect]);
        assert!(chat.skipping(), "the server waits to ask");
        assert!(chat.load_answered(), "now it asks");
    }

    #[test]
    fn stopping_before_anything_is_told_says_the_turns_before_arent_shown() {
        let mut chat = resuming();
        chat.act(ChatAction::Stop);
        let notice = chat.node.notice.clone().unwrap_or_default();
        assert!(notice.contains("the turns before aren't shown"), "{notice}");
    }

    #[test]
    fn an_agent_gone_mid_load_shows_the_message_held_and_ends_the_passing_over() {
        let mut chat = resuming();
        send(&mut chat, "held");
        chat.disconnected(None);
        assert_eq!(prompts(&chat), ["held"]);
        assert!(matches!(
            outcome(&chat.node.turns[0]),
            Some(Outcome::Failed(_))
        ));
        assert!(
            chat.loading(),
            "nothing came: it loads with the next message"
        );
        let mut chat = resuming();
        replay_user(&mut chat, "before");
        chat.act(ChatAction::Stop);
        chat.disconnected(None);
        assert!(
            !chat.skipping(),
            "nothing more comes of a load whose agent has gone"
        );
    }

    #[test]
    fn nothing_told_again_counts_as_running_or_as_one_of_clackatoos_tools() {
        let mut chat = resuming();
        replay_user(&mut chat, "edit it");
        replay(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"t","title":"Edit a.txt","kind":"edit",
                "status":"in_progress","rawInput":{"server":"clackatoo"}}"#,
        );
        assert!(!chat.writing(), "it ran long ago");
        assert!(chat.own_tools.is_empty(), "{:?}", chat.own_tools);
    }

    #[test]
    fn an_agent_started_again_loads_anew_only_if_nothing_came() {
        let mut chat = resuming();
        assert!(chat.agent_restarted());
        assert!(chat.loading(), "nothing came: loaded from the new process");
        replay_user(&mut chat, "before");
        assert!(chat.agent_restarted());
        assert!(!chat.loading(), "partly told: resumed, not told twice");
    }

    #[test]
    fn a_chat_handed_over_mid_load_keeps_what_came_as_heard() {
        let mut chat = resuming();
        replay_user(&mut chat, "before");
        send(&mut chat, "held");
        let handed = chat.hand_over();
        assert_eq!(
            handed.known, 1,
            "what was told again is heard; the message held never went"
        );
        assert_eq!(handed.turns.len(), 2);
    }
}
