//! Chats with agents: what a chat's view holds, the patches that keep it
//! current, and what a client can ask of a chat.
//!
//! A chat is a list of turns: each a message the user sent, and what the
//! agent did with it, in order: its words, its thinking and the tools it
//! used. The agent's words are Markdown, which clients lay out for
//! themselves, at whatever width they have.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::wire;

/// A chat with an agent, as a view's node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatNode {
    /// The agent the chat talks to, as it's shown: "Claude Code",
    /// "Codex"…
    pub agent: String,
    /// The turns, oldest first.
    pub turns: Vec<Turn>,
    /// What the chat is doing.
    pub state: ChatState,
    /// What the agent is asking, waiting for an answer.
    pub question: Option<Question>,
    /// Something to tell the user that belongs to no turn, such as that
    /// the agent stopped while the chat was idle.
    pub notice: Option<String>,
    /// The agent's settings for the chat, such as its mode and model, as
    /// the agent offers them.
    pub settings: Vec<Setting>,
    /// How much of the agent's context the chat fills, once it has said.
    pub usage: Option<Usage>,
    /// The other agents the chat could move to, and their models, for
    /// the model menu to offer beside its own agent's.
    pub providers: Vec<Provider>,
    /// The chats its agent started, oldest first: clients show them as a
    /// card in the turn that started them, and open one as a pane.
    pub subagents: Vec<Subagent>,
    /// User messages waiting to run, shared by every window.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_QUEUE }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_QUEUE }, _, _>"
    )]
    pub delivery: ChatDelivery,
    /// Latest account quota snapshots for the daemon's configured providers.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::PROVIDER_USAGE }, _>",
        deserialize_with = "wire::since::<{ wire::PROVIDER_USAGE }, _, _>"
    )]
    pub provider_usage: Vec<ProviderUsage>,
    /// Absolute index of the first loaded turn. Zero for a complete history.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_HISTORY }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_HISTORY }, _, _>"
    )]
    pub first_turn: u32,
    /// User-message turns before this range (handoff dividers do not count).
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_HISTORY }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_HISTORY }, _, _>"
    )]
    pub first_message: u32,
    /// Daemon-owned background work belonging to this chat.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::HARNESS }, _>",
        deserialize_with = "wire::since::<{ wire::HARNESS }, _, _>"
    )]
    pub activities: Vec<crate::Activity>,
    /// What the agent left running in the background, such as shells it
    /// started, newest last: those still running, and those that ended
    /// since the user last sent a message.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::AGENT_TASKS }, _>",
        deserialize_with = "wire::since::<{ wire::AGENT_TASKS }, _, _>"
    )]
    pub tasks: Vec<AgentTask>,
    /// The agent's plan, as it last gave it: its to-do list, in order,
    /// such as Claude Code's `TodoWrite` and Codex's plan. Empty if it
    /// has none.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::AGENT_PLAN }, _>",
        deserialize_with = "wire::since::<{ wire::AGENT_PLAN }, _, _>"
    )]
    pub plan: Vec<PlanStep>,
    /// The slash commands the agent offers, as it last said: its own,
    /// its skills and its MCP servers' prompts, each typed as `/name`
    /// at the start of a message. Empty until it has said.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::AGENT_COMMANDS }, _>",
        deserialize_with = "wire::since::<{ wire::AGENT_COMMANDS }, _, _>"
    )]
    pub commands: Vec<AgentCommand>,
    /// What the agent says as it tries its request to the model again,
    /// as "Retrying Claude, attempt 2 of 10.", while the turn answering
    /// waits on that: `None` once it's going again, or not answering.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_FACTS }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_FACTS }, _, _>"
    )]
    pub retrying: Option<String>,
    /// The messages the user sent, oldest first, each once, for Up to
    /// recall in the box: the chat's own turns' (see
    /// [`Turn::user_prompt`]), the newest [`PROMPTS_KEPT`] that fit in
    /// [`PROMPTS_BYTES`]. Nothing but the turns: the daemon works them out
    /// again from its turns after an upgrade (see [`recent_prompts`]), and
    /// it and every client keep them as turns come (see
    /// [`ChatNode::apply`]), so a snapshot needn't hold every turn for
    /// them all to be recalled.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::PROMPT_BOX }, _>",
        deserialize_with = "wire::since::<{ wire::PROMPT_BOX }, _, _>"
    )]
    pub prompts: Vec<String>,
    /// The message being typed in the chat's box and not sent yet, as a
    /// client last said: text only, at most [`DRAFT_LONGEST`] bytes. A
    /// client opening the chat with nothing typed starts with it. Changes
    /// to it aren't sent to viewers as they're made, as typing isn't
    /// mirrored from box to box: only sending, which empties it (see
    /// [`ChatPatch::DraftCleared`]). `None` from a daemon that keeps no
    /// drafts, as an older one, the chat's or the one lending it: what the
    /// draft is isn't known, which isn't the same as there being none.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::PROMPT_BOX }, _>",
        deserialize_with = "wire::since::<{ wire::PROMPT_BOX }, _, _>"
    )]
    pub draft: Option<String>,
    /// Where it works, made absolute on the daemon's machine, for a window
    /// to write a file it mentions from there (see [`crate::mentions`]).
    /// `None` from a daemon older than protocol 119.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_PATHS }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_PATHS }, _, _>"
    )]
    pub paths: Option<ChatPaths>,
}

/// Where a chat works, made absolute on the daemon's machine.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPaths {
    /// Its agent's working directory.
    pub cwd: String,
    /// Its project: the git repository the working directory is in, or
    /// else that directory.
    pub project: String,
}

/// How many of the user's messages a chat keeps for Up to recall, as
/// [`ChatNode::prompts`].
pub const PROMPTS_KEPT: usize = 200;

/// The most bytes of them it keeps, so a chat's snapshot stays small.
pub const PROMPTS_BYTES: usize = 128 * 1024;

/// The longest message kept for Up to recall, in bytes: longer is surely
/// pasted, and too much to send with every snapshot.
pub const PROMPT_LONGEST: usize = 16 * 1024;

/// The longest draft a chat keeps, in bytes: as long as a message may be.
pub const DRAFT_LONGEST: usize = 64 * 1024;

/// Whether `prompt` is one Up recalls: not blank, nor longer than
/// [`PROMPT_LONGEST`].
fn recallable(prompt: &str) -> bool {
    !prompt.trim().is_empty() && prompt.len() <= PROMPT_LONGEST
}

/// The user's messages in `turns`, oldest first, as [`ChatNode::prompts`]
/// keeps them: each once, where it was sent last, the newest
/// [`PROMPTS_KEPT`] that fit in [`PROMPTS_BYTES`] together. It's what
/// [`remember_prompt`] makes of them one at a time, worked out from the
/// newest back.
#[must_use]
pub fn recent_prompts(turns: &[Turn]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut kept = Vec::new();
    let mut bytes = 0;
    for prompt in turns.iter().rev().filter_map(Turn::user_prompt) {
        if !recallable(prompt) || !seen.insert(prompt) {
            continue;
        }
        if kept.len() == PROMPTS_KEPT || bytes + prompt.len() > PROMPTS_BYTES {
            break;
        }
        bytes += prompt.len();
        kept.push(prompt.to_owned());
    }
    kept.reverse();
    kept
}

/// Adds `prompt` to `prompts`, kept as [`ChatNode::prompts`] are: as the
/// newest, moved there if it was kept before, and the oldest let go past
/// [`PROMPTS_KEPT`] or [`PROMPTS_BYTES`].
pub fn remember_prompt(prompts: &mut Vec<String>, prompt: &str) {
    if !recallable(prompt) {
        return;
    }
    prompts.retain(|kept| kept != prompt);
    prompts.push(prompt.to_owned());
    let mut bytes: usize = prompts.iter().map(String::len).sum();
    let mut drop = prompts.len().saturating_sub(PROMPTS_KEPT);
    for gone in &prompts[..drop] {
        bytes -= gone.len();
    }
    while bytes > PROMPTS_BYTES {
        bytes -= prompts[drop].len();
        drop += 1;
    }
    prompts.drain(..drop);
}

/// A slash command an agent offers, typed as `/name` at the start of a
/// message, with what follows it as its input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCommand {
    /// Its name, without the `/`: "compact", "review".
    pub name: String,
    /// What it does, as the agent says, on one line.
    pub description: String,
    /// What to type after it, if it takes anything, as the agent hints
    /// it: "branch name".
    pub hint: Option<String>,
}

/// The slash command `text`, a message, starts with, if it starts with
/// one, and what follows it: `/review main` is `("review", "main")`. A
/// command's name is what follows the `/` up to white space, with no
/// other `/` in it, so a path such as `/etc/hosts` isn't one. The
/// agents' adapters read one so: from the start of the message, white
/// space before it skipped.
#[must_use]
pub fn slash_command(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim_start().strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    let valid = !name.is_empty() && !name.contains('/') && !name.chars().any(char::is_control);
    valid.then(|| (name, rest[end..].trim()))
}

/// The commands Claude Code's and Codex's own interfaces have that their
/// adapters don't: typed in a chat, they'd reach the model as words.
/// Clients do what each is for themselves, or say what to use instead,
/// unless the chat's agent offers a command of the name, which is the
/// agent's.
pub const INTERFACE_COMMANDS: &[&str] = &[
    "clear",
    "new",
    "resume",
    "rewind",
    "context",
    "status",
    "cost",
    "usage",
    "diff",
    "model",
    "export",
    "exit",
    "quit",
    "help",
    "config",
    "permissions",
    "login",
    "logout",
    "theme",
    "vim",
    "terminal-setup",
];

/// The command a client does itself, of [`INTERFACE_COMMANDS`], that
/// `text`, a message to a chat whose agent offers `offered`, starts
/// with, if any, as that list names it, and what follows it: one the
/// agent offers is the agent's. Names are matched ignoring case. What
/// follows is for the client to judge: `/help me find the bug` is words
/// for the agent, not `/help`.
#[must_use]
pub fn interface_command<'a>(
    text: &'a str,
    offered: &[AgentCommand],
) -> Option<(&'static str, &'a str)> {
    let (name, rest) = slash_command(text)?;
    let ours = INTERFACE_COMMANDS
        .iter()
        .find(|ours| ours.eq_ignore_ascii_case(name))?;
    let theirs = (offered.iter()).any(|command| command.name.eq_ignore_ascii_case(name));
    (!theirs).then_some((ours, rest))
}

/// Commands Claude Code's and Codex's adapters do whether or not they've
/// said so yet, as `available_commands_update` may not have come.
pub const KNOWN_COMMANDS: &[&str] = &[
    "compact",
    "init",
    "review",
    "review-branch",
    "review-commit",
    "plan",
    "goal",
    "mcp",
    "skills",
    "status",
    "context",
    "heapdump",
    "extra-usage",
    "usage",
    "clear",
    "resume",
    "rename",
    "logout",
];

/// Whether `text`, a message to a chat whose agent offers `offered`, is
/// a command the agent's adapter reads as one: a name it offers, or one
/// of [`KNOWN_COMMANDS`] if it hasn't said what it offers. `/usr is
/// full` isn't one.
#[must_use]
pub fn agent_command(text: &str, offered: &[AgentCommand]) -> bool {
    let Some((name, _)) = slash_command(text) else {
        return false;
    };
    if offered.is_empty() {
        KNOWN_COMMANDS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(name))
    } else {
        (offered.iter()).any(|command| command.name.eq_ignore_ascii_case(name))
    }
}

/// One step of an agent's plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    /// What the step is, as the agent says: "Write the tests".
    pub content: String,
    /// How far it's got.
    pub state: PlanState,
}

/// How far a [`PlanStep`] has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanState {
    /// Not started.
    ToDo,
    /// Being worked on.
    Doing,
    /// Done.
    Done,
}

/// Work an agent runs in the background of a chat, as the agent reports
/// it: a shell Claude Code started with `run_in_background`, a monitor, or
/// a turn it took by itself, such as to answer a shell that finished.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTask {
    /// The agent's name for it, which asking to stop it gives back.
    pub id: String,
    /// What kind of work it is.
    pub kind: AgentTaskKind,
    /// What it's for, as the agent says: "Start the dev server".
    pub title: String,
    /// The command it runs, if it's a shell and the chat saw the tool call
    /// that started it.
    pub command: Option<String>,
    /// How it's going.
    pub state: AgentTaskState,
    /// What the agent said of how it ended, if it said anything.
    pub summary: Option<String>,
    /// When the daemon heard of it, in milliseconds since the Unix epoch.
    pub started: u64,
    /// When it ended, if it has.
    pub ended: Option<u64>,
    /// Whether the agent can be asked to stop it.
    pub can_stop: bool,
    /// The file on the daemon's machine its output is written to, as the
    /// agent said, if it did: Claude Code writes each background shell's
    /// output to a `tasks/ID.output` file, which its own `TaskOutput` tool
    /// reads. ACP carries no output itself, only this path, so a window
    /// follows the file in a terminal on that machine.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::TASK_OUTPUT }, _>",
        deserialize_with = "wire::since::<{ wire::TASK_OUTPUT }, _, _>"
    )]
    pub output: Option<String>,
}

impl AgentTask {
    /// Whether it's still going: running, paused or being stopped.
    #[must_use]
    pub fn going(&self) -> bool {
        matches!(
            self.state,
            AgentTaskState::Running | AgentTaskState::Paused | AgentTaskState::Stopping
        )
    }

    /// The program and its arguments, run with no shell, that a terminal
    /// on the daemon's machine runs to follow its output, if the agent said
    /// where that goes: `tail`, the program at `tail` (a path, or a name
    /// looked up on the daemon's `PATH`), from the last
    /// [`TASK_OUTPUT_TAIL`] bytes, following the file as it grows, and
    /// waiting for it if it isn't there yet. The path comes after `--`, so
    /// it's never taken for an option. The terminal reads what it prints as
    /// it would any program's, colours and all.
    #[must_use]
    pub fn follow_output(&self, tail: &str) -> Option<Vec<String>> {
        let path = self.output.as_ref()?;
        let bytes = TASK_OUTPUT_TAIL.to_string();
        Some(
            [tail, "-c", &bytes, "-F", "--", path]
                .map(str::to_owned)
                .to_vec(),
        )
    }
}

/// The most of a background task's output a terminal following it shows
/// to begin with, in bytes (see [`AgentTask::follow_output`]): what came
/// before is in the file, which the agent reads whole.
pub const TASK_OUTPUT_TAIL: u64 = 256 * 1024;

/// What kind of background work an [`AgentTask`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentTaskKind {
    /// A shell command.
    Shell,
    /// Something the agent watches, such as a command's output.
    Monitor,
    /// A workflow of the agent's own.
    Workflow,
    /// A turn the agent took by itself, with no message from the user.
    Turn,
    /// Something else.
    Other,
}

/// How an [`AgentTask`] is going.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentTaskState {
    /// It's running.
    Running,
    /// The agent paused it.
    Paused,
    /// The user asked for it to stop, and the agent hasn't said it has.
    Stopping,
    /// It finished.
    Done,
    /// It failed.
    Failed,
    /// It was stopped.
    Stopped,
    /// Its agent stopped, or the chat started again, before it said how
    /// it ended.
    Lost,
}

/// One provider account's quota readings. Context belongs to the chat, not here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUsage {
    /// Configured provider identity.
    pub agent: String,
    /// Display name.
    pub title: String,
    /// Last successful reading, in Unix milliseconds; zero means none yet.
    pub checked: u64,
    /// A read-only refresh is in progress.
    pub refreshing: bool,
    /// Every quota window reported by this provider, bounded to 32.
    pub limits: Vec<UsageLimit>,
    /// Unavailable, refresh failure, or adapter-specific explanation.
    pub notice: Option<String>,
}

/// A subscription allowance, including model-specific and spend allowances.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLimit {
    /// Stable provider-local window identity.
    pub id: String,
    /// Human-readable window label.
    pub label: String,
    /// Percentage used, from zero through 100.
    pub used: u8,
    /// Reset time in Unix milliseconds, when reported.
    pub resets: Option<u64>,
}

/// The daemon's pending user messages. Image bytes stay in the daemon.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDelivery {
    /// Messages in delivery order.
    pub queued: Vec<QueuedMessage>,
    /// A stop, failure or upgrade paused automatic delivery.
    pub paused: bool,
    /// A native steering request awaiting acknowledgment.
    pub sending: Option<u64>,
    /// This session supports steering without cancelling the current turn.
    pub native_steering: bool,
}

/// A pending message, addressed by a stable chat-local ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMessage {
    /// Stable until sent or removed.
    pub id: u64,
    /// The user's text.
    pub text: String,
    /// Number of attached images.
    pub images: u8,
    /// Optional provider and model to use when this message runs.
    pub target: Option<ChatTarget>,
}

/// A provider choice attached to a message, applied only at delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTarget {
    /// Configured provider name.
    pub agent: String,
    /// Model identifier, or the provider's default.
    pub model: Option<String>,
    /// Context carry-over budget, in tokens.
    pub budget: u32,
}

/// A chat another chat's agent started, as the chat that started it shows
/// it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagent {
    /// The chat.
    pub chat: crate::TerminalId,
    /// Its agent, as it's shown: "Claude Code".
    pub agent: String,
    /// The profile it was started with, such as `explore`, if one.
    pub profile: Option<String>,
    /// The model it runs on, as shown, once that's known.
    pub model: Option<String>,
    /// What it was asked to do: its first message's first line, cut short.
    pub task: String,
    /// How it's going.
    pub status: SubagentStatus,
    /// When it was started, in milliseconds since the Unix epoch.
    pub started: u64,
    /// When it last finished, if it isn't working now.
    pub ended: Option<u64>,
    /// The turn of the chat that started it that it was started in, by
    /// its index: the card goes there.
    pub turn: u32,
    /// Whether it's in a pane of its own, rather than in the background.
    pub shown: bool,
    /// What its agent runs in the background now, for the card to say.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::AGENT_TASKS }, _>",
        deserialize_with = "wire::since::<{ wire::AGENT_TASKS }, _, _>"
    )]
    pub tasks: Vec<AgentTask>,
}

/// How a chat another chat's agent started is going.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentStatus {
    /// Its agent is starting.
    Starting,
    /// Its agent is answering.
    Working,
    /// Its agent asks the user something, and waits: it must be opened to
    /// answer.
    Asking,
    /// It finished, and waits for another message.
    Done,
    /// Its last turn went wrong, or its agent couldn't start: why.
    Failed(String),
    /// It was closed.
    Closed,
}

impl SubagentStatus {
    /// Whether it's still going: starting, working or asking.
    #[must_use]
    pub fn going(&self) -> bool {
        matches!(self, Self::Starting | Self::Working | Self::Asking)
    }
}

/// Another agent a chat could move to, and the models it offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    /// Its name, to switch to it by: `codex`.
    pub agent: String,
    /// What to show: "Codex".
    pub title: String,
    /// Its models, as far as they're known.
    pub models: ProviderModels,
}

/// What's known of another agent's models.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderModels {
    /// Nothing yet: opening the model menu asks the daemon to find out.
    Unknown,
    /// The daemon is asking the agent.
    Learning,
    /// These, as the agent offers them.
    Known(Vec<SettingChoice>),
    /// It isn't installed: picking it offers to install it.
    NotInstalled,
    /// The agent couldn't say: why. Picking it switches anyway, and the
    /// chat says what's wrong.
    Failed(String),
}

/// One of the agent's settings for a chat, and the values it can take.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    /// What the agent calls it, to change it with.
    pub id: String,
    /// What to show: "Mode", "Model"…
    pub name: String,
    /// What it's about.
    pub kind: SettingKind,
    /// The value it has, by its ID.
    pub value: String,
    /// The values it can take.
    pub choices: Vec<SettingChoice>,
}

/// What a setting is about, so clients know where to put it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingKind {
    /// How much the agent may do without asking, such as Claude Code's
    /// Manual, Accept edits, Plan and Auto.
    Mode,
    /// The model it runs on.
    Model,
    /// How hard it thinks.
    Effort,
    /// What the agent may do with clackatoo's own tools: a setting of
    /// the daemon's, not the agent's.
    Tools,
    /// Anything else.
    Other,
    /// Whether the agent gets tools that drive clackatoo's browser: a
    /// setting of the daemon's, not the agent's.
    Browser,
}

/// One value a setting can take.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingChoice {
    /// What the agent calls it.
    pub value: String,
    /// What to show: "Auto", "Sonnet 5.5"…
    pub name: String,
    /// More about it, if the agent says: "Fastest for quick answers".
    pub description: Option<String>,
}

/// How much of the agent's context a chat fills, and of its account's
/// limits, where the agent says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens in the context.
    pub used: u64,
    /// The most it holds: 0 if the agent doesn't say.
    pub size: u64,
    /// The account's limit over five hours, once the agent has said how
    /// much of it is used, as Claude Code's does for a subscription.
    /// `None` from a daemon older than protocol 81.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::LIMITS }, _>",
        deserialize_with = "wire::since::<{ wire::LIMITS }, _, _>"
    )]
    pub five_hour: Option<Limit>,
    /// Its limit over a week, likewise.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::LIMITS }, _>",
        deserialize_with = "wire::since::<{ wire::LIMITS }, _, _>"
    )]
    pub weekly: Option<Limit>,
    /// What the session has cost so far, where the agent says: Claude
    /// Code's adapter does.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_FACTS }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_FACTS }, _, _>"
    )]
    pub cost: Option<Cost>,
}

/// An amount of money an agent says something cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// How much, in millionths of the currency: a dollar is 1 000 000.
    pub micros: u64,
    /// The currency, as its ISO 4217 code in capitals: `*b"USD"`.
    pub currency: [u8; 3],
}

impl Cost {
    /// The cost of `amount` in `currency`, as an agent says it: `None`
    /// for an amount that's negative, not a number or past any bill, or
    /// a currency that isn't three letters.
    #[must_use]
    pub fn of(amount: f64, currency: &str) -> Option<Self> {
        let code: [u8; 3] = currency.as_bytes().try_into().ok()?;
        if !code.iter().all(u8::is_ascii_alphabetic) || !amount.is_finite() || amount < 0.0 {
            return None;
        }
        let micros = (amount * 1_000_000.0).round();
        // A million a session is no agent's.
        if micros > 1e12 {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked finite, not negative and small above"
        )]
        let micros = micros as u64;
        Some(Self {
            micros,
            currency: code.map(|byte| byte.to_ascii_uppercase()),
        })
    }

    /// The cost as it's shown: "$0.42", "$0.0031" below a cent, or
    /// "1.20 EUR" in another currency.
    #[must_use]
    pub fn label(&self) -> String {
        let amount = if self.micros > 0 && self.micros < 10_000 {
            let shown = format!("0.{:06}", self.micros);
            shown.trim_end_matches('0').to_owned()
        } else {
            let cents = (self.micros + 5_000) / 10_000;
            format!("{}.{:02}", cents / 100, cents % 100)
        };
        match &self.currency {
            b"USD" => format!("${amount}"),
            code => format!("{amount} {}", String::from_utf8_lossy(code)),
        }
    }

    /// What's been spent since `before`, in the same currency: all of it
    /// if nothing was before; `None` if before was in another currency,
    /// or more, as when the agent started again.
    #[must_use]
    pub fn since(&self, before: Option<Self>) -> Option<Self> {
        let Some(before) = before else {
            return Some(*self);
        };
        (before.currency == self.currency && before.micros <= self.micros).then(|| Self {
            micros: self.micros - before.micros,
            currency: self.currency,
        })
    }
}

impl Usage {
    /// How much of the context is used, in percent, from 0 to 100, if the
    /// agent says how much it holds. More used than it holds, as an agent
    /// may say, is 100.
    #[must_use]
    pub fn percent(&self) -> Option<u8> {
        (self.size > 0).then(|| {
            let percent = u128::from(self.used) * 100 / u128::from(self.size);
            u8::try_from(percent.min(100)).unwrap_or(100)
        })
    }
}

/// How much of one of an account's limits is used, as its agent says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limit {
    /// How much, in percent, from 0 to 100: 100 once it's reached.
    pub used: u8,
    /// When it starts again, in milliseconds since the Unix epoch, if the
    /// agent says.
    pub resets: Option<u64>,
}

/// Past this much of a chat's context, or of one of its account's limits,
/// used, in percent, clients show it in amber...
pub const AMBER: u8 = 80;

/// ...and past this, in red.
pub const RED: u8 = 95;

/// How full a chat's context, or one of its account's limits, is, as
/// clients show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fullness {
    /// Room to spare.
    Room,
    /// Past [`AMBER`].
    Filling,
    /// Past [`RED`].
    Full,
}

impl Fullness {
    /// How full `percent` used is.
    #[must_use]
    pub fn of(percent: u8) -> Self {
        if percent > RED {
            Self::Full
        } else if percent > AMBER {
            Self::Filling
        } else {
            Self::Room
        }
    }
}

/// One line of what clients say of a chat's usage in full: what it's
/// about, what's said of it, and how full that is, if it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageLine {
    /// "Context", "5-hour limit"…
    pub name: String,
    /// "84k of 200k tokens, 42%", "reached, resets at 15:40"…
    pub said: String,
    /// How full it is: `None` for a context of a size not said.
    pub fullness: Option<Fullness>,
}

impl Usage {
    /// What a meter of it says: "42% context", or for an agent that
    /// doesn't say how much its context holds, how much it's used, "84k
    /// context".
    #[must_use]
    pub fn label(&self) -> String {
        match self.percent() {
            Some(percent) => format!("{percent}% context"),
            None => format!("{} context", tokens(self.used)),
        }
    }

    /// The account's limits, by name, as known at `now`, in milliseconds
    /// since the Unix epoch: one said to reset by then has, and how much
    /// of it is used now isn't known until the agent says again, as it
    /// says only when that changes.
    #[must_use]
    pub fn limits(&self, now: u64) -> [(&'static str, Option<Limit>); 2] {
        let known = |limit: Option<Limit>| {
            limit.filter(|limit| limit.resets.is_none_or(|resets| resets > now))
        };
        [
            ("5-hour", known(self.five_hour)),
            ("Weekly", known(self.weekly)),
        ]
    }

    /// The fullest of the account's limits known at `now`, if it's full
    /// enough to say so beside a meter, "5-hour 91%", and how full that
    /// is.
    #[must_use]
    pub fn pressing(&self, now: u64) -> Option<(String, Fullness)> {
        let (name, limit) = (self.limits(now).into_iter())
            .filter_map(|(name, limit)| Some((name, limit?)))
            .max_by_key(|(_, limit)| limit.used)?;
        let fullness = Fullness::of(limit.used);
        (fullness > Fullness::Room).then(|| (format!("{name} {}%", limit.used), fullness))
    }

    /// What's said of it in full, a line each: the context, and each
    /// limit known at `now`, with when it resets as `resets` says a time,
    /// both in milliseconds since the Unix epoch: "at 15:40".
    #[must_use]
    pub fn detail(&self, now: u64, resets: impl Fn(u64) -> String) -> Vec<UsageLine> {
        let percent = self.percent();
        let context = match percent {
            Some(percent) => format!(
                "{} of {} tokens, {percent}%",
                tokens(self.used),
                tokens(self.size)
            ),
            None => format!("{} tokens", tokens(self.used)),
        };
        let mut lines = vec![UsageLine {
            name: "Context".to_owned(),
            said: context,
            fullness: percent.map(Fullness::of),
        }];
        for (name, limit) in self.limits(now) {
            let Some(limit) = limit else {
                continue;
            };
            let mut said = if limit.used >= 100 {
                "reached".to_owned()
            } else {
                format!("{}% used", limit.used)
            };
            if let Some(at) = limit.resets {
                let _ = write!(said, ", resets {}", resets(at));
            }
            lines.push(UsageLine {
                name: format!("{name} limit"),
                said,
                fullness: Some(Fullness::of(limit.used)),
            });
        }
        lines
    }
}

/// A count of tokens, short: "830", "38k", "1M", "1.2M".
#[must_use]
pub fn tokens(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{}k", (count + 500) / 1_000),
        _ => {
            let tenths = count.saturating_add(50_000) / 100_000;
            if tenths.is_multiple_of(10) {
                format!("{}M", tenths / 10)
            } else {
                format!("{}.{}M", tenths / 10, tenths % 10)
            }
        }
    }
}

impl Setting {
    /// The choice it has now, if it's one of its choices.
    #[must_use]
    pub fn current(&self) -> Option<&SettingChoice> {
        self.choices
            .iter()
            .find(|choice| choice.value == self.value)
    }

    /// Whether it's Codex's Plan mode: its adapter's `collaboration_mode`,
    /// Default or Plan, which is of no kind clackatoo knows (`Other`), as
    /// its category is Codex's own, but goes with the mode where clients
    /// show settings.
    #[must_use]
    pub fn is_plan_mode(&self) -> bool {
        self.kind == SettingKind::Other
            && self.id == "collaboration_mode"
            && self.choices.iter().any(|choice| choice.value == "plan")
    }

    /// Whether it's [Codex's Plan mode](Self::is_plan_mode), in Plan mode.
    #[must_use]
    pub fn planning(&self) -> bool {
        self.is_plan_mode() && self.value == "plan"
    }
}

/// A message the user sent, and what the agent did with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// The message. Empty for a turn holding a conversation from before
    /// chats were kept as turns.
    pub prompt: String,
    /// When it was sent, in milliseconds since the Unix epoch.
    pub sent: u64,
    /// What the agent did, in order.
    pub items: Vec<Item>,
    /// How the turn ended, once it has.
    pub end: Option<TurnEnd>,
    /// What the agent changed in files in the turn, if it changed any.
    pub files: Option<TurnFiles>,
    /// The chat whose agent sent the message, if another agent sent it
    /// rather than the user: clients show it beside the message.
    pub from: Option<Sender>,
    /// Where the chat moved to another agent: a turn of its own, with no
    /// message and nothing done, that's shown as a line across the chat.
    pub switch: Option<Switch>,
    /// Small previews retained for this message, never sent to the agent.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_PREVIEWS }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_PREVIEWS }, _, _>"
    )]
    pub previews: Vec<ChatPreview>,
    /// The agent took the turn by itself, with no message, as Claude Code
    /// does when something it ran in the background ends: why, as the
    /// agent said, or empty if it didn't. Its `prompt` is empty.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::AGENT_TURNS }, _>",
        deserialize_with = "wire::since::<{ wire::AGENT_TURNS }, _, _>"
    )]
    pub woke: Option<String>,
}

impl Turn {
    /// The message the user typed, if the turn is one the user sent: not
    /// another chat's agent's, nor a notice of clackatoo's (both have a
    /// sender), nor one the agent took by itself, nor a move to another
    /// agent, nor one with no words, such as an image alone.
    #[must_use]
    pub fn user_prompt(&self) -> Option<&str> {
        let own = self.from.is_none() && self.switch.is_none() && self.woke.is_none();
        (own && !self.prompt.trim().is_empty()).then_some(self.prompt.as_str())
    }
}

/// The chat whose agent sent a message to another chat, as it was when it
/// sent it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    /// The chat.
    pub chat: crate::TerminalId,
    /// Its agent, as it's shown: "Claude Code", "Codex"…
    pub agent: String,
    /// Its title.
    pub title: String,
}

/// A chat moving to another agent, or back to one it had before.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Switch {
    /// The agent it moved from, as shown: "Claude Code".
    pub from: String,
    /// The agent it moved to, as shown: "Codex".
    pub to: String,
    /// The model it runs on there, as shown, once that's known.
    pub model: Option<String>,
    /// Whether it went back to a session it had with that agent before,
    /// which hears only what happened since, rather than starting one that
    /// hears the conversation so far.
    pub resumed: bool,
    /// Settings that didn't carry over, as "Mode Auto isn't Codex's: it
    /// uses its own default".
    pub notes: Vec<String>,
}

impl Switch {
    /// What the line across the chat says: "Switched to Codex · 6 Luna ·
    /// context carried over".
    #[must_use]
    pub fn label(&self) -> String {
        let mut label = if self.resumed {
            format!("Switched back to {}", self.to)
        } else {
            format!("Switched to {}", self.to)
        };
        if let Some(model) = &self.model {
            let _ = write!(label, " · {model}");
        }
        label.push_str(if self.resumed {
            " · caught up on what happened since"
        } else {
            " · context carried over"
        });
        label
    }
}

/// What an agent changed in files in a turn, and whether it can be undone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFiles {
    /// How many files it changed, made or deleted.
    pub count: u32,
    /// Where they are.
    pub state: TurnFilesState,
}

/// Whether a turn's changes to files are made, or undone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnFilesState {
    /// Made, as the agent made them: they can be undone.
    Made,
    /// Undone: they can be made again.
    Undone,
    /// Undone in some files, as forced where others couldn't be.
    Partly,
    /// Too old: the daemon no longer keeps what they were, so they can't
    /// be undone.
    TooOld,
}

/// How a turn ended, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEnd {
    /// When, in milliseconds since the Unix epoch.
    pub at: u64,
    /// How.
    pub outcome: Outcome,
    /// What the agent itself said of a failure, word for word, where
    /// that isn't what [`Outcome::Failed`] says: that's put in plain
    /// words, with what to do.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_FACTS }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_FACTS }, _, _>"
    )]
    pub raw: Option<String>,
    /// What the turn spent, where the agent says.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_FACTS }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_FACTS }, _, _>"
    )]
    pub spent: Option<Spent>,
}

/// What a turn spent, as its agent says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spent {
    /// Tokens the model read, those read from its cache included.
    pub input: u64,
    /// Tokens it wrote.
    pub output: u64,
    /// Of those it read, how many came from its cache.
    pub cached: u64,
    /// What it cost, if the agent says.
    pub cost: Option<Cost>,
}

impl Spent {
    /// What's shown under the turn: "12k tokens · $0.04", without a cost
    /// of nothing, as a turn stopped before it said anything costs.
    #[must_use]
    pub fn label(&self) -> String {
        let tokens = self.input.saturating_add(self.output);
        let mut label = format!("{} tokens", self::tokens(tokens));
        if let Some(cost) = self.cost.filter(|cost| cost.micros > 0) {
            let _ = write!(label, " · {}", cost.label());
        }
        label
    }
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// The agent finished.
    Done,
    /// The user stopped it.
    Stopped,
    /// It went wrong: why, to show the user.
    Failed(String),
}

/// One thing the agent did in a turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Item {
    /// Its words, in Markdown.
    Words(String),
    /// Its thinking, in Markdown.
    Thought(String),
    /// A tool it used.
    Tool(Tool),
    /// Something newer than the client, which a newer daemon sent in place
    /// of it, as text to show: what it was.
    Other(String),
    /// The agent compacted its context to make room, the conversation
    /// before giving way to a summary of it: shown as a line across the
    /// turn, with what the agent says of it, as "automatic · 182k → 31k
    /// tokens", or nothing.
    Compacted(String),
    /// Something a subagent the agent started itself did, as Claude
    /// Code's Task tool starts one: its words, its thinking or a tool it
    /// used, inside the tool call that started it. Items stay in the
    /// order they came, so patches keep their places; clients show each
    /// under its tool. Never a nested item itself: a subagent's own
    /// subagent names the tool call that started it.
    Nested {
        /// The ID of the tool call that started the subagent.
        parent: String,
        /// What it did: words, thinking or a tool, never anything else.
        #[serde(deserialize_with = "nestable")]
        item: Box<Item>,
    },
}

/// What a nested item can hold, read as `Item`'s first three variants
/// are written: reading one never goes deeper, so a peer can't send a
/// nesting deep enough to overflow the stack.
#[derive(Deserialize)]
enum Nestable {
    Words(String),
    Thought(String),
    Tool(Tool),
}

fn nestable<'de, D: serde::Deserializer<'de>>(reader: D) -> Result<Box<Item>, D::Error> {
    Ok(Box::new(match Nestable::deserialize(reader)? {
        Nestable::Words(words) => Item::Words(words),
        Nestable::Thought(words) => Item::Thought(words),
        Nestable::Tool(tool) => Item::Tool(tool),
    }))
}

impl Item {
    /// `item`, as something the subagent the tool call `parent` started
    /// did. What's already nested is nested under `parent` instead, and
    /// anything but words, thinking or a tool goes as words, as only
    /// those can be read nested.
    #[must_use]
    pub fn nested(parent: String, item: Self) -> Self {
        let item = match item.flattened() {
            Self::Other(text) => Self::Words(text),
            Self::Compacted(details) => Self::Words(Self::compacted_label(&details)),
            item => item,
        };
        Self::Nested {
            parent,
            item: Box::new(item),
        }
    }

    /// The ID of the tool call whose subagent did it, if a subagent did.
    #[must_use]
    pub fn parent(&self) -> Option<&str> {
        match self {
            Self::Nested { parent, .. } => Some(parent),
            _ => None,
        }
    }

    /// What it is, nested or not.
    #[must_use]
    pub fn inner(&self) -> &Self {
        match self {
            Self::Nested { item, .. } => item,
            item => item,
        }
    }

    /// What it is, nested or not, to change.
    pub fn inner_mut(&mut self) -> &mut Self {
        match self {
            Self::Nested { item, .. } => item,
            item => item,
        }
    }

    /// The tool it is, nested or not, if it's one.
    #[must_use]
    pub fn tool(&self) -> Option<&Tool> {
        match self.inner() {
            Self::Tool(tool) => Some(tool),
            _ => None,
        }
    }

    /// The tool it is, nested or not, if it's one, to change.
    pub fn tool_mut(&mut self) -> Option<&mut Tool> {
        match self.inner_mut() {
            Self::Tool(tool) => Some(tool),
            _ => None,
        }
    }

    /// The same with nothing nested, as a peer from before nesting reads
    /// it: a subagent's work as the agent's own.
    #[must_use]
    pub fn flattened(self) -> Self {
        match self {
            Self::Nested { item, .. } => *item,
            item => item,
        }
    }

    /// What the line an [`Item::Compacted`] with `details` is shown as
    /// says.
    #[must_use]
    pub fn compacted_label(details: &str) -> String {
        if details.is_empty() {
            "Context compacted".to_owned()
        } else {
            format!("Context compacted · {details}")
        }
    }
}

/// A tool the agent used, such as reading a file or running a command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// What the agent calls it, to find it again.
    pub id: String,
    /// What it does, as the agent describes it: "Read Cargo.toml".
    pub title: String,
    /// How it's going.
    pub status: ToolStatus,
    /// What sort of tool it is.
    pub kind: ToolKind,
    /// What it shows: what it's for, its output, or the changes it makes
    /// to files.
    pub content: Vec<ToolContent>,
    /// Where in files it works, as the agent says: a change to a file is
    /// at the line its location gives.
    pub locations: Vec<Location>,
    /// The code the command it ran exited with, where the agent says.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_FACTS }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_FACTS }, _, _>"
    )]
    pub exit: Option<i32>,
}

/// A file a tool works in, and where in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// The file's path.
    pub path: String,
    /// The line, from 1, if the agent says.
    pub line: Option<u32>,
}

/// What sort of tool an agent used, as it says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    /// Reads files.
    Read,
    /// Changes files.
    Edit,
    /// Deletes files.
    Delete,
    /// Moves or renames files.
    Move,
    /// Searches files.
    Search,
    /// Runs a command: its title is the command.
    Execute,
    /// Thinks.
    Think,
    /// Fetches from the web.
    Fetch,
    /// Something else.
    #[default]
    Other,
}

/// Something a tool shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolContent {
    /// Text, in Markdown: a command's output comes fenced, as code.
    Text(String),
    /// A change to a file.
    Diff(Diff),
    /// A command's output as it ran, as plain text, streamed apart from
    /// the tool's other content, as Codex's adapter streams it.
    Output(String),
    /// Something newer than the client, which a newer daemon sent in place
    /// of it, as text to show, in Markdown.
    Other(String),
    /// A bounded thumbnail of an image returned by a tool.
    Image(ChatPreview),
}

impl Tool {
    /// Adds `text` to the end of its output.
    pub fn append_output(&mut self, text: &str) {
        match self.output_mut() {
            Some(output) => output.push_str(text),
            None => self.content.push(ToolContent::Output(text.to_owned())),
        }
    }

    /// Its output, if it has any.
    pub fn output_mut(&mut self) -> Option<&mut String> {
        self.content.iter_mut().find_map(|content| match content {
            ToolContent::Output(output) => Some(output),
            _ => None,
        })
    }
}

/// A change to a file: the part of it changed, before and after.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    /// The file's path.
    pub path: String,
    /// What was there, or `None` for a new file.
    pub old: Option<String>,
    /// What's there now.
    pub new: String,
}

/// How a tool call is going.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    /// Still running.
    Running,
    /// Finished.
    Done,
    /// Failed.
    Failed,
}

/// What a chat is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatState {
    /// The agent is starting, or opening the chat's session.
    #[default]
    Starting,
    /// Waiting for a message.
    Ready,
    /// The agent is answering.
    Answering,
    /// The agent couldn't start: why. The next message tries again.
    Failed(String),
}

/// The agent asking whether it may use a tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// Its number, which an answer gives back, so an answer meant for one
    /// question never lands on the next.
    pub id: u64,
    /// What it wants to do, as the agent describes it: "Write notes.txt".
    /// Or, for the daemon's own question, the whole question.
    pub title: String,
    /// The answers it offers.
    pub choices: Vec<Choice>,
    /// Whether the daemon asks it, for itself, as whether to install the
    /// agent, and the title is the whole question, to show as it is.
    /// Every other question is about what the agent would do, and is
    /// shown as asking whether to let it, however the title is worded:
    /// an agent can word one like the daemon's.
    #[serde(default)]
    pub own: bool,
    /// What the agent asks the user to fill in or open, if it's that
    /// rather than whether it may use a tool (ACP's elicitation), answered
    /// with [`ChatAction::Fill`]. Its `choices` then only decline: a
    /// window older than protocol 113 shows the title and those.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::ELICITATION }, _>",
        deserialize_with = "wire::since::<{ wire::ELICITATION }, _, _>"
    )]
    pub form: Option<crate::Form>,
    /// The plan the agent asks to go ahead with, in Markdown, if the tool
    /// it asks about is one that leaves planning, as Claude Code's
    /// `ExitPlanMode`: to show above the choices.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::ELICITATION }, _>",
        deserialize_with = "wire::since::<{ wire::ELICITATION }, _, _>"
    )]
    pub plan: Option<String>,
    /// The ID of the agent's tool call it asks about, if it asks about
    /// one: a window opens the cards of the tools whose subagents it's
    /// folded under, so what's asked about shows.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::NESTED_WORK }, _>",
        deserialize_with = "wire::since::<{ wire::NESTED_WORK }, _, _>"
    )]
    pub tool: Option<String>,
}

/// A block of code in an agent's words, to colour: see
/// [`ClientMessage::Colour`](crate::ClientMessage::Colour).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Code {
    /// Its language, as its fence names it: `rust`, `py`.
    pub language: String,
    /// The code.
    pub text: String,
}

impl Question {
    /// Its title in pieces, as clients show it: words, and code where it
    /// quotes some, as Markdown's inline code does, between runs of
    /// backticks. The daemon quotes what an agent wrote that way (see
    /// [`code_span`]).
    #[must_use]
    pub fn said(&self) -> Vec<Said> {
        said(&self.title)
    }
}

/// A piece of a question's title: see [`Question::said`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Said {
    /// Words, to show as they are.
    Words(String),
    /// Code, quoted: to show in the terminal's font.
    Code(String),
}

/// `text` quoted as Markdown's inline code: between runs of backticks
/// longer than any in it, so nothing in it can end the code early and
/// pass for what's said around it, with a space inside each where
/// Markdown would otherwise take one away, or read a backtick of the
/// text as part of the run.
#[must_use]
pub fn code_span(text: &str) -> String {
    let longest = (text.split(|ch| ch != '`'))
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let spaced = text.starts_with(' ') && text.ends_with(' ') && !text.trim().is_empty();
    if text.starts_with('`') || text.ends_with('`') || spaced {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

/// `text` in pieces, as Markdown reads its inline code: a run of
/// backticks starts code that the next run of as many ends; with none,
/// the run is just backticks.
#[must_use]
pub fn said(text: &str) -> Vec<Said> {
    let mut pieces = Vec::new();
    let mut words = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        words.push_str(&rest[..start]);
        let run = rest[start..].len() - rest[start..].trim_start_matches('`').len();
        let after = &rest[start + run..];
        let Some(end) = closing(after, run) else {
            words.push_str(&rest[start..start + run]);
            rest = after;
            continue;
        };
        if !words.is_empty() {
            pieces.push(Said::Words(std::mem::take(&mut words)));
        }
        let code = after[..end].replace(['\n', '\r'], " ");
        let stripped = code.len() >= 2
            && code.starts_with(' ')
            && code.ends_with(' ')
            && !code.trim().is_empty();
        let code = if stripped {
            code[1..code.len() - 1].to_owned()
        } else {
            code
        };
        pieces.push(Said::Code(code));
        rest = &after[end + run..];
    }
    words.push_str(rest);
    if !words.is_empty() {
        pieces.push(Said::Words(words));
    }
    pieces
}

/// Where in `text` the first run of exactly `run` backticks starts.
fn closing(text: &str, run: usize) -> Option<usize> {
    let mut at = 0;
    while let Some(found) = text[at..].find('`') {
        let start = at + found;
        let len = text[start..].len() - text[start..].trim_start_matches('`').len();
        if len == run {
            return Some(start);
        }
        at = start + len;
    }
    None
}

/// One answer to a question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// What the agent calls it, to answer with.
    pub id: String,
    /// What to show: "Allow", "Always allow"…
    pub name: String,
    /// What it means.
    pub kind: ChoiceKind,
}

/// What an answer means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChoiceKind {
    /// Yes, this once.
    AllowOnce,
    /// Yes, and don't ask again.
    AllowAlways,
    /// No, this once.
    RejectOnce,
    /// No, and don't ask again.
    RejectAlways,
    /// An answer this build doesn't know the meaning of, shown as the
    /// agent names it: never taken for yes or for no, nor chosen for the
    /// user.
    Other,
}

/// The ID of the answer a daemon gives a window from before
/// [`ChoiceKind::Other`] in place of a question's answers, if all are of
/// that kind, which it leaves out: it says no, cancelling the question.
pub const DECLINE: &str = "clackatoo.decline";

/// A chat as the list of chats shows it: enough to say which need the
/// user, without each one's turns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSummary {
    /// The chat.
    pub chat: crate::TerminalId,
    /// Its title.
    pub title: String,
    /// Its agent, as it's shown: "Claude Code".
    pub agent: String,
    /// What it's doing.
    pub state: ChatState,
    /// Whether its agent asks the user something, and waits.
    pub asking: bool,
    /// Whether a turn ended, or a question came, while no one present was
    /// looking at it, and no one has looked since.
    pub unseen: bool,
    /// The chat whose agent started it, if one did.
    pub parent: Option<crate::TerminalId>,
    /// The workspace it's in: its pane's, or for a chat in no pane, that
    /// of the nearest chat above it that's in one.
    pub workspace: Option<crate::WorkspaceId>,
    /// When something last happened in it, in milliseconds since the Unix
    /// epoch: its last message sent, or its last turn's end.
    pub active: u64,
    /// The profile it was started as, such as `explore`, if an agent
    /// started it as one. `None` from a daemon older than protocol 109.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::SUBAGENT_PROFILE }, _>",
        deserialize_with = "wire::since::<{ wire::SUBAGENT_PROFILE }, _, _>"
    )]
    pub profile: Option<String>,
    /// Whether its last turn went wrong (ended `Outcome::Failed`), for a
    /// notification of it to say so. `None` from a daemon older than
    /// protocol 116, which doesn't say.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::TURN_FAILED }, _>",
        deserialize_with = "wire::since::<{ wire::TURN_FAILED }, _, _>"
    )]
    pub failed: Option<bool>,
}

impl ChatSummary {
    /// Whether it needs the user: it asks something, or has something
    /// they haven't seen.
    #[must_use]
    pub fn needs_you(&self) -> bool {
        self.asking || self.unseen
    }
}

/// What a new chat could be started with, as the start page `:Agent`
/// opens offers it, and the chats it could show instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatChoices {
    /// Every agent, with its models as far as they're known.
    pub agents: Vec<Provider>,
    /// The agent a chat talks to when none is named, by its name.
    pub default: String,
    /// The directory a chat beside the pane asked about would work in:
    /// the one its program or file is in, or else the home directory.
    pub cwd: String,
    /// The home directory.
    pub home: String,
    /// The chats in no pane, summed up: chats agents started in the
    /// background, and those started by scripts.
    pub unshown: Vec<ChatSummary>,
}

/// A session an agent kept, as it lists them: one of clackatoo's chats',
/// or one started elsewhere, as in a terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// The agent's ID for it.
    pub session: String,
    /// What the agent calls it, often its first message. Empty if it
    /// gives it no title.
    pub title: String,
    /// The directory it works in.
    pub cwd: String,
    /// When it last changed, as the agent says it: RFC 3339, as
    /// `2026-10-13T20:54:20Z`.
    pub updated: Option<String>,
    /// The chat that has it open now, if one does: showing that chat is
    /// resuming it.
    pub chat: Option<crate::TerminalId>,
}

/// A session an agent kept, for a new chat to go on with instead of
/// starting one: see [`crate::NewAgent::resume`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeSession {
    /// The agent's ID for it.
    pub session: String,
    /// What the agent calls it, for the chat's title until the agent
    /// names it. Empty for none.
    pub title: String,
}

/// One change to a chat a client already has.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatPatch {
    /// The turn at `index` is now `turn`. An index one past the last adds
    /// a turn.
    Turn {
        /// Which turn.
        index: u32,
        /// What it is now: boxed, as a turn is far bigger than any other
        /// change.
        turn: Box<Turn>,
    },
    /// Item `item` of turn `turn` is now `value`, as a tool is as it
    /// goes. An index one past the last adds an item.
    Item {
        /// The turn's index.
        turn: u32,
        /// The item's index in the turn.
        item: u32,
        /// What it is now.
        value: Item,
    },
    /// More of the agent's words, or its thinking: `text` goes on the end
    /// of item `item` of turn `turn`.
    Append {
        /// The turn's index.
        turn: u32,
        /// The item's index in the turn.
        item: u32,
        /// What's added.
        text: String,
    },
    /// More of a command's output: `text` goes on the end of the output
    /// of the tool that's item `item` of turn `turn`.
    Output {
        /// The turn's index.
        turn: u32,
        /// The tool's index in the turn.
        item: u32,
        /// What's added.
        text: String,
    },
    /// The chat's state is now this.
    State(ChatState),
    /// The agent's question is now this.
    Question(Option<Question>),
    /// The notice is now this.
    Notice(Option<String>),
    /// The settings are now these.
    Settings(Vec<Setting>),
    /// The usage is now this.
    Usage(Option<Usage>),
    /// What turn `turn` changed in files is now `files`.
    Files {
        /// The turn's index.
        turn: u32,
        /// What it changed.
        files: Option<TurnFiles>,
    },
    /// The chat talks to this agent now, as it's shown.
    Agent(String),
    /// The other agents it could move to are now these.
    Providers(Vec<Provider>),
    /// The chats its agent started are now these.
    Subagents(Vec<Subagent>),
    /// The user-message queue changed.
    Delivery(ChatDelivery),
    /// Account quotas across the daemon's providers.
    ProviderUsage(Vec<ProviderUsage>),
    /// A contiguous page preceding already loaded turns. Absolute indices
    /// keep live patches and file actions stable while history is prepended.
    History {
        /// First absolute turn in this page.
        first: u32,
        /// Exclusive end, equal to the receiver's first loaded turn.
        before: u32,
        /// User-message turns preceding this page.
        messages_before: u32,
        /// Older turns, oldest first.
        turns: Vec<Turn>,
    },
    /// Complete bounded activity list for this chat.
    Activities(Vec<crate::Activity>),
    /// What the agent runs in the background is now this.
    Tasks(Vec<AgentTask>),
    /// The agent's plan is now this.
    Plan(Vec<PlanStep>),
    /// The slash commands the agent offers are now these.
    Commands(Vec<AgentCommand>),
    /// What the agent says as it tries again is now this.
    Retrying(Option<String>),
    /// A client sent `sent` from the chat's box, which emptied the draft
    /// (see [`ChatNode::draft`]): every other box holding just that,
    /// ignoring white space at its ends, empties too, and one holding
    /// anything else is left as it is.
    DraftCleared {
        /// The message sent.
        sent: String,
    },
}

/// A small, decoded image for chat history. Shared by snapshots and paint
/// caches without copying pixels. Originals are fetched separately by handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPreview {
    /// Width in pixels, at most `SIDE`.
    pub width: u32,
    /// Height in pixels, at most `SIDE`.
    pub height: u32,
    /// Straight-alpha RGBA pixels, row by row.
    pub rgba: std::sync::Arc<[u8]>,
    /// Opaque original-image handle, scoped to this chat. Zero if unavailable.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_ORIGINALS }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_ORIGINALS }, _, _>"
    )]
    pub original: u64,
}

impl ChatPreview {
    /// Maximum thumbnail dimension.
    pub const SIDE: u32 = 256;
    /// Keep recent thumbnails within four MiB per chat. Older messages
    /// retain their attachment labels when their previews are evicted.
    pub const HISTORY_BYTES: usize = 4 * 1024 * 1024;

    /// Checks all dimensions before a renderer or daemon uses the pixels.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= Self::SIDE
            && self.height <= Self::SIDE
            && self.rgba.len() == (self.width as usize) * (self.height as usize) * 4
    }
}

/// A pasted PNG, encoded once by the window, ready for ACP. Kept only
/// until sent; the transcript retains only its small preview and dimensions,
/// with an opaque handle for fetching the separately retained original.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatImage {
    /// PNG bytes as standard base64.
    pub data: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// A bounded thumbnail made by the clipboard worker.
    #[serde(
        default,
        skip_serializing_if = "wire::before::<{ wire::CHAT_PREVIEWS }, _>",
        deserialize_with = "wire::since::<{ wire::CHAT_PREVIEWS }, _, _>"
    )]
    pub preview: Option<ChatPreview>,
}

impl ChatImage {
    /// At most four images in one message.
    pub const MAX_COUNT: usize = 4;
    /// Encoded images together fit comfortably within a protocol frame.
    pub const MAX_DATA: usize = 8 * 1024 * 1024;
    /// A decoded clipboard image is at most 64 MiB of RGBA pixels.
    pub const MAX_PIXELS: u64 = 16 * 1024 * 1024;

    /// Checks dimensions and the encoded size before processing data.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && u64::from(self.width) * u64::from(self.height) <= Self::MAX_PIXELS
            && !self.data.is_empty()
            && self.data.len() <= Self::MAX_DATA
            && self.preview.as_ref().is_none_or(ChatPreview::valid)
    }

    /// What the draft and the transcript say about this attachment.
    #[must_use]
    pub fn label(&self) -> String {
        format!("Image · {} × {}", self.width, self.height)
    }
}

/// What a client asks of a chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatAction {
    /// Send this message, starting a turn.
    Send(String),
    /// Stop the agent answering.
    Stop,
    /// Answer the agent's question numbered `question` with the choice
    /// with this ID, or with none. An answer to a question no longer
    /// asked is ignored.
    Answer {
        /// The question's number.
        question: u64,
        /// The choice's ID.
        option: Option<String>,
    },
    /// Change one of the agent's settings for the chat.
    Set {
        /// The setting's ID.
        setting: String,
        /// The value to give it, by its ID.
        value: String,
    },
    /// Undo what the agent changed in files in turn `turn`, keeping what
    /// anyone changed since. If any file can't be, because a change since
    /// touches the same lines, none is, unless `force`, which undoes the
    /// rest.
    Undo {
        /// The turn's index.
        turn: u32,
        /// Undo what can be, though some files can't be.
        force: bool,
    },
    /// Make again what undoing turn `turn` took out of files, as `Undo`.
    Redo {
        /// The turn's index.
        turn: u32,
        /// Redo what can be, though some files can't be.
        force: bool,
    },
    /// Move the chat to another agent, on `model`, or the agent's own
    /// choice: a session with it hears the conversation so far, or what
    /// happened since, if it had one before, with its next message. Not
    /// while the agent is answering.
    Switch {
        /// The agent, by its name.
        agent: String,
        /// The model, by its value, as the agent offers it.
        model: Option<String>,
        /// About the most of the conversation it hears, in tokens.
        budget: u32,
    },
    /// Find out the models of the other agents the chat could move to,
    /// where they aren't known, as the model menu opens.
    LearnModels,
    /// Show `chat`, one this chat's agent started, in a pane beside this
    /// chat, or point it out if it's in one already.
    Open {
        /// The chat to show.
        chat: crate::TerminalId,
    },
    /// Send text and clipboard images together. Requires `wire::CHAT_IMAGES`.
    SendImages {
        /// The accompanying message, which may be empty.
        text: String,
        /// PNG attachments, bounded by `ChatImage`'s limits.
        images: Vec<ChatImage>,
    },
    /// Switch provider and send one prompt as a single accepted operation.
    /// Requires `wire::CHAT_HANDOFF`. A refused switch never sends the prompt.
    SendTo {
        /// Destination agent's configured name.
        agent: String,
        /// Destination model, or its default.
        model: Option<String>,
        /// Maximum context carry-over in tokens.
        budget: u32,
        /// Message accompanying the handoff.
        text: String,
        /// Optional bounded image attachments.
        images: Vec<ChatImage>,
    },
    /// Accept a user message atomically; queue it while the agent is busy.
    Submit {
        /// Accompanying text; may be empty with images.
        text: String,
        /// Bounded PNG attachments.
        images: Vec<ChatImage>,
        /// A provider change to apply with this message.
        target: Option<ChatTarget>,
        /// Deliver to the active work now instead of waiting behind it.
        steer: bool,
    },
    /// Remove a message that has not been delivered. Stale IDs are refused.
    RemoveQueued {
        /// The message's stable ID.
        id: u64,
    },
    /// Deliver a queued message now, resuming a paused queue.
    SendQueued {
        /// The message's stable ID.
        id: u64,
        /// Steer the active work if it is still running.
        steer: bool,
    },
    /// Refresh account quotas without sending a model prompt. Calls are paced.
    RefreshUsage {
        /// Ask to refresh a cached reading, subject to the minimum interval.
        force: bool,
    },
    /// Ask the agent to stop work it runs in the background. Requires
    /// `wire::AGENT_TASKS`.
    StopTask {
        /// The task, by the agent's name for it.
        id: String,
    },
    /// Answer the agent's question numbered `question`, a form it asks to
    /// be filled in or a link to open (see [`Question::form`]). An answer
    /// to a question no longer asked, or that doesn't fit the form, is
    /// ignored. Requires `wire::ELICITATION`.
    Fill {
        /// The question's number.
        question: u64,
        /// The answer.
        reply: crate::FormReply,
    },
    /// Send the last turn's message, which failed, again, with its
    /// images, as a turn of its own. Requires `wire::CHAT_FACTS`.
    Resend,
    /// Keep `text` as the message being typed in the chat's box and not
    /// sent yet, for a client opening the chat with nothing typed to start
    /// with (see [`ChatNode::draft`]): the last said is kept, whichever
    /// client said it. Empty for none; at most [`DRAFT_LONGEST`] bytes.
    /// Requires `wire::PROMPT_BOX`.
    Draft {
        /// What's typed.
        text: String,
    },
    /// Take the message `queued`, waiting in the queue, out of it and into
    /// the chat's draft, before what the draft held, in one step: answered
    /// with the message's text, `Response::Text`, for the client asking to
    /// put before what's typed in its own box. Refused for a message with images or one that
    /// moves the chat to another agent, which a draft can't hold. Requires
    /// `wire::PROMPT_BOX`.
    TakeBack {
        /// The queued message's ID.
        queued: u64,
    },
}

/// The number turn `index` of `turns` goes by, as people count them: from
/// 1, counting only turns with a message, not those where the chat moved
/// to another agent. `None` for one of those, or past the end.
#[must_use]
pub fn turn_number(turns: &[Turn], index: usize) -> Option<usize> {
    let turn = turns.get(index)?;
    let before = turns[..index].iter().filter(|turn| turn.switch.is_none());
    turn.switch.is_none().then(|| before.count() + 1)
}

/// The index of the turn [`turn_number`] numbers `number`, if there is one.
#[must_use]
pub fn turn_numbered(turns: &[Turn], number: usize) -> Option<usize> {
    (turns.iter().enumerate())
        .filter(|(_, turn)| turn.switch.is_none())
        .nth(number.checked_sub(1)?)
        .map(|(index, _)| index)
}

impl ChatNode {
    /// The start of a page ending at local index `before`: at most eight
    /// whole turns and about 512 KiB. A single larger turn remains intact.
    #[must_use]
    pub fn history_start(&self, before: usize) -> usize {
        let before = before.min(self.turns.len());
        let mut first = before;
        let mut bytes = 0usize;
        for index in (before.saturating_sub(8)..before).rev() {
            let size =
                postcard::experimental::serialized_size(&self.turns[index]).unwrap_or(usize::MAX);
            if first < before && bytes.saturating_add(size) > 512 * 1024 {
                break;
            }
            bytes = bytes.saturating_add(size);
            first = index;
        }
        first
    }

    /// A page before an absolute boundary, or none if it is outside this node.
    #[must_use]
    pub fn history_page(&self, before: u32) -> Option<ChatPatch> {
        let end = before.checked_sub(self.first_turn)? as usize;
        if end == 0 || end > self.turns.len() {
            return None;
        }
        let start = self.history_start(end);
        let first = self.first_turn.checked_add(u32::try_from(start).ok()?)?;
        let messages_before = self.first_message.checked_add(
            u32::try_from(
                self.turns[..start]
                    .iter()
                    .filter(|turn| turn.switch.is_none())
                    .count(),
            )
            .ok()?,
        )?;
        Some(ChatPatch::History {
            first,
            before,
            messages_before,
            turns: self.turns[start..end].to_vec(),
        })
    }

    /// Copy metadata and a suffix without cloning the unloaded history.
    #[must_use]
    pub fn since_turn(&self, first: Option<u32>) -> Self {
        let start = first
            .map_or_else(
                || self.history_start(self.turns.len()),
                |first| first.saturating_sub(self.first_turn) as usize,
            )
            .min(self.turns.len());
        Self {
            agent: self.agent.clone(),
            turns: self.turns[start..].to_vec(),
            state: self.state.clone(),
            question: self.question.clone(),
            notice: self.notice.clone(),
            settings: self.settings.clone(),
            usage: self.usage,
            providers: self.providers.clone(),
            subagents: self.subagents.clone(),
            delivery: self.delivery.clone(),
            provider_usage: self.provider_usage.clone(),
            activities: self.activities.clone(),
            tasks: self.tasks.clone(),
            plan: self.plan.clone(),
            commands: self.commands.clone(),
            retrying: self.retrying.clone(),
            prompts: self.prompts.clone(),
            draft: self.draft.clone(),
            paths: self.paths.clone(),
            first_turn: self
                .first_turn
                .saturating_add(u32::try_from(start).unwrap_or(u32::MAX)),
            first_message: self.first_message.saturating_add(
                u32::try_from(
                    self.turns[..start]
                        .iter()
                        .filter(|t| t.switch.is_none())
                        .count(),
                )
                .unwrap_or(u32::MAX),
            ),
        }
    }

    /// Applies `patch`. Changes to turns or items the chat doesn't have
    /// are ignored, so a confused daemon can't crash a client.
    pub fn apply(&mut self, patch: ChatPatch) {
        match patch {
            ChatPatch::Turn { index, turn } => {
                let Some(index) = index.checked_sub(self.first_turn).map(|i| i as usize) else {
                    return;
                };
                match index.cmp(&self.turns.len()) {
                    std::cmp::Ordering::Less => self.turns[index] = *turn,
                    // A message the user sent, from whichever client, is
                    // the newest for Up to recall.
                    std::cmp::Ordering::Equal => {
                        if let Some(prompt) = turn.user_prompt() {
                            remember_prompt(&mut self.prompts, prompt);
                        }
                        self.turns.push(*turn);
                    }
                    std::cmp::Ordering::Greater => {}
                }
            }
            ChatPatch::Item { turn, item, value } => {
                if let Some(turn) = self
                    .turns
                    .get_mut(turn.wrapping_sub(self.first_turn) as usize)
                {
                    let item = item as usize;
                    match item.cmp(&turn.items.len()) {
                        std::cmp::Ordering::Less => turn.items[item] = value,
                        std::cmp::Ordering::Equal => turn.items.push(value),
                        std::cmp::Ordering::Greater => {}
                    }
                }
            }
            ChatPatch::Append { turn, item, text } => {
                if let Some(Item::Words(words) | Item::Thought(words)) = self
                    .turns
                    .get_mut(turn.wrapping_sub(self.first_turn) as usize)
                    .and_then(|turn| turn.items.get_mut(item as usize))
                    .map(Item::inner_mut)
                {
                    words.push_str(&text);
                }
            }
            ChatPatch::Output { turn, item, text } => {
                if let Some(tool) = self
                    .turns
                    .get_mut(turn.wrapping_sub(self.first_turn) as usize)
                    .and_then(|turn| turn.items.get_mut(item as usize))
                    .and_then(Item::tool_mut)
                {
                    tool.append_output(&text);
                }
            }
            ChatPatch::State(state) => self.state = state,
            ChatPatch::Question(question) => self.question = question,
            ChatPatch::Notice(notice) => self.notice = notice,
            ChatPatch::Settings(settings) => self.settings = settings,
            ChatPatch::Usage(usage) => self.usage = usage,
            ChatPatch::Files { turn, files } => {
                if let Some(turn) = self
                    .turns
                    .get_mut(turn.wrapping_sub(self.first_turn) as usize)
                {
                    turn.files = files;
                }
            }
            ChatPatch::Agent(agent) => self.agent = agent,
            ChatPatch::Providers(providers) => self.providers = providers,
            ChatPatch::Subagents(subagents) => self.subagents = subagents,
            ChatPatch::Delivery(delivery) => self.delivery = delivery,
            ChatPatch::ProviderUsage(usage) => self.provider_usage = usage,
            ChatPatch::Activities(activities) => self.activities = activities,
            ChatPatch::Tasks(tasks) => self.tasks = tasks,
            ChatPatch::Plan(plan) => self.plan = plan,
            ChatPatch::Commands(commands) => self.commands = commands,
            ChatPatch::Retrying(retrying) => self.retrying = retrying,
            ChatPatch::DraftCleared { .. } => self.draft = Some(String::new()),
            ChatPatch::History {
                first,
                before,
                messages_before,
                mut turns,
            } => {
                if before == self.first_turn
                    && first < before
                    && turns.len() == (before - first) as usize
                {
                    turns.append(&mut self.turns);
                    self.turns = turns;
                    self.first_turn = first;
                    self.first_message = messages_before;
                }
            }
        }
    }

    /// The chat as plain text: each message and what the agent said.
    #[must_use]
    pub fn text(&self) -> String {
        let mark = |status| match status {
            ToolStatus::Running => "…",
            ToolStatus::Done => "✓",
            ToolStatus::Failed => "✗",
        };
        let mut text = String::new();
        for turn in &self.turns {
            if let Some(switch) = &turn.switch {
                let _ = write!(text, "— {} —\n\n", switch.label());
            }
            if !turn.prompt.is_empty() {
                text.push_str("> ");
                text.push_str(&turn.prompt.replace('\n', "\n> "));
                text.push_str("\n\n");
            }
            for item in &turn.items {
                match item {
                    Item::Words(words) | Item::Other(words) => {
                        text.push_str(words.trim_end());
                        text.push_str("\n\n");
                    }
                    Item::Thought(_) => {}
                    Item::Compacted(details) => {
                        let _ = write!(text, "— {} —\n\n", Item::compacted_label(details));
                    }
                    Item::Tool(tool) => {
                        let _ = write!(text, "{} {}\n\n", mark(tool.status), tool.title);
                    }
                    // A subagent's tools, under the tool that started it.
                    // Its words went to the agent, which says what counts.
                    Item::Nested { item, .. } => {
                        if let Item::Tool(tool) = &**item {
                            let _ = write!(text, "  ↳ {} {}\n\n", mark(tool.status), tool.title);
                        }
                    }
                }
            }
            if let Some(TurnEnd {
                outcome: Outcome::Failed(why),
                ..
            }) = &turn.end
            {
                let _ = write!(text, "({why})\n\n");
            }
        }
        text.truncate(text.trim_end().len());
        text
    }
}

/// What the subagent a tool call started did in a turn, as clients sum it
/// up on that tool: see [`Turn::nested_work`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NestedWork {
    /// How many of its items there are.
    pub items: usize,
    /// How many tools it used.
    pub tools: usize,
    /// The index in the turn of the last of its items.
    pub last: usize,
    /// How many files it, or a subagent of its own, started to change,
    /// make, delete or move, however that went: what's folded away under
    /// its card that may have changed something is never unsaid.
    pub files: usize,
    /// How many commands it, or a subagent of its own, started.
    pub commands: usize,
    /// How many other tools that may change something, as an MCP
    /// server's, it, or a subagent of its own, started.
    pub others: usize,
}

/// What a tool an agent started may have changed, by its kind: see
/// [`Tool::effect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// It runs a command.
    Command,
    /// It changes, makes, deletes or moves files.
    Files,
    /// Something else, as an MCP server's tool: what isn't known.
    Other,
}

impl Tool {
    /// What it may have changed once started, however it went: `None`
    /// for reads, searches, fetches and thinking, and for the rows
    /// clackatoo adds itself.
    #[must_use]
    pub fn effect(&self) -> Option<Effect> {
        if self.id.starts_with("clackatoo-") {
            return None;
        }
        match self.kind {
            ToolKind::Execute => Some(Effect::Command),
            ToolKind::Edit | ToolKind::Delete | ToolKind::Move => Some(Effect::Files),
            ToolKind::Other => Some(Effect::Other),
            ToolKind::Read | ToolKind::Search | ToolKind::Fetch | ToolKind::Think => None,
        }
    }
}

/// How deep [`Turn::nested_work`] follows subagents' subagents: a peer
/// could say each is the other's.
const DEEPEST_NESTED: usize = 8;

impl Turn {
    /// What the subagents of the turn's tool calls did, by the ID of the
    /// tool call that started each: its items and tools only what each
    /// did itself, the files and commands its own subagents' too. Empty,
    /// and cheap, for a turn with nothing nested.
    #[must_use]
    pub fn nested_work(&self) -> std::collections::HashMap<&str, NestedWork> {
        use std::collections::{HashMap, HashSet};
        let mut work: HashMap<&str, NestedWork> = HashMap::new();
        if self.items.iter().all(|item| item.parent().is_none()) {
            return work;
        }
        let parents: HashMap<&str, Option<&str>> = (self.items.iter())
            .filter_map(|item| Some((item.tool()?.id.as_str(), item.parent())))
            .collect();
        let mut files: HashMap<&str, HashSet<&str>> = HashMap::new();
        for (at, item) in self.items.iter().enumerate() {
            let Some(parent) = item.parent() else {
                continue;
            };
            let sum = work.entry(parent).or_default();
            sum.items += 1;
            sum.tools += usize::from(item.tool().is_some());
            sum.last = at;
            let Some((tool, effect)) = item.tool().and_then(|tool| Some((tool, tool.effect()?)))
            else {
                continue;
            };
            let changed: Vec<&str> = match effect {
                Effect::Files => {
                    let diffs = (tool.content.iter()).filter_map(|content| match content {
                        ToolContent::Diff(diff) => Some(diff.path.as_str()),
                        _ => None,
                    });
                    let places = tool.locations.iter().map(|place| place.path.as_str());
                    let mut paths: Vec<&str> = diffs.chain(places).collect();
                    if paths.is_empty() {
                        paths.push(tool.id.as_str());
                    }
                    paths
                }
                Effect::Command | Effect::Other => Vec::new(),
            };
            // Said on its card, and every card it's folded under.
            let mut above = Some(parent);
            for _ in 0..DEEPEST_NESTED {
                let Some(id) = above else { break };
                files.entry(id).or_default().extend(&changed);
                let sum = work.entry(id).or_default();
                sum.commands += usize::from(effect == Effect::Command);
                sum.others += usize::from(effect == Effect::Other);
                above = parents.get(id).copied().flatten();
            }
        }
        for (id, paths) in files {
            work.entry(id).or_default().files = paths.len();
        }
        work
    }

    /// The line a tool call whose subagent did `work` says it with:
    /// "↳ Reading src/main.rs · 3 tool uses" while `tool`, the call, runs,
    /// what the subagent does now; then "↳ Done · 12 tool uses", and what
    /// it changed and ran, "· edited 2 files · ran 1 command".
    #[must_use]
    pub fn nested_line(&self, tool: &Tool, work: &NestedWork) -> String {
        let now = match tool.status {
            ToolStatus::Done => "Done".to_owned(),
            ToolStatus::Failed => "Failed".to_owned(),
            ToolStatus::Running => match self.items.get(work.last).map(Item::inner) {
                Some(Item::Tool(tool)) => tool.doing(),
                Some(Item::Thought(_)) => "Thinking".to_owned(),
                Some(Item::Words(words)) => {
                    let line = words.lines().rfind(|line| !line.trim().is_empty());
                    let line = line.unwrap_or_default().trim();
                    match line.char_indices().nth(LONGEST_DOING) {
                        Some((at, _)) => format!("{}…", line[..at].trim_end()),
                        None => line.to_owned(),
                    }
                }
                _ => "Working".to_owned(),
            },
        };
        let plural = |count: usize, one: &str, many: &str| {
            if count == 1 {
                format!("1 {one}")
            } else {
                format!("{count} {many}")
            }
        };
        let mut line = format!("↳ {now}");
        if work.tools > 0 {
            let _ = write!(line, " · {}", plural(work.tools, "tool use", "tool uses"));
        }
        if work.files > 0 {
            let _ = write!(line, " · edited {}", plural(work.files, "file", "files"));
        }
        if work.commands > 0 {
            let _ = write!(
                line,
                " · ran {}",
                plural(work.commands, "command", "commands")
            );
        }
        if work.others > 0 {
            let _ = write!(
                line,
                " · used {}",
                plural(work.others, "other tool", "other tools")
            );
        }
        line
    }
}

/// The longest a tool's title is said in full on the line that shows an
/// agent working, in characters: longer, it's cut short, with an
/// ellipsis, so the seconds after it still fit on a phone.
const LONGEST_DOING: usize = 60;

/// Verbs agents start tools' titles with, and how the line that shows an
/// agent working says them: "Read Cargo.toml" is "Reading Cargo.toml".
const DOING_VERBS: [(&str, &str); 14] = [
    ("Read", "Reading"),
    ("Edit", "Editing"),
    ("Write", "Writing"),
    ("Delete", "Deleting"),
    ("Move", "Moving"),
    ("Search", "Searching"),
    ("Find", "Finding"),
    ("Fetch", "Fetching"),
    ("List", "Listing"),
    ("Run", "Running"),
    ("Open", "Opening"),
    ("Create", "Creating"),
    ("Update", "Updating"),
    ("Check", "Checking"),
];

impl ChatNode {
    /// The turn going, if one is: the last, until it ends.
    #[must_use]
    pub fn going(&self) -> Option<&Turn> {
        self.turns.last().filter(|turn| turn.end.is_none())
    }

    /// Whether the agent is at work: starting, or answering a message.
    /// Not while it's [asking](Self::asking) the user something, as then
    /// it waits for them, nor once it couldn't start.
    #[must_use]
    pub fn working(&self) -> bool {
        !self.asking()
            && match self.state {
                ChatState::Starting => true,
                ChatState::Answering => self.going().is_some(),
                ChatState::Ready | ChatState::Failed(_) => false,
            }
    }

    /// Whether the agent waits for the user to answer its question, and
    /// does nothing meanwhile. The daemon's own question while it
    /// answers, which only suggests installing the agent it runs another
    /// way, isn't waited on: the agent goes on with its work.
    #[must_use]
    pub fn asking(&self) -> bool {
        (self.question.as_ref())
            .is_some_and(|question| !(question.own && self.state == ChatState::Answering))
    }

    /// When the turn whose seconds clients count, while the agent works,
    /// was sent, in milliseconds since the Unix epoch: the turn going's.
    /// Not while it loads the session, as each turn it tells again comes
    /// as a turn going of its own, nor for a turn from before turns said
    /// when they were sent.
    #[must_use]
    pub fn counted_from(&self) -> Option<u64> {
        if !self.working() || self.loading() {
            return None;
        }
        self.going().map(|turn| turn.sent).filter(|&sent| sent > 0)
    }

    /// What the agent is doing, while it's [working](Self::working), as
    /// clients say it: "Starting Claude Code", "Loading the session",
    /// "Thinking", "Running: cargo test", "Reading Cargo.toml", "Writing"
    /// while its answer comes, or else "Working". No ellipsis: the dots
    /// beside it say it's going on.
    #[must_use]
    pub fn doing(&self) -> Option<String> {
        if !self.working() {
            return None;
        }
        if self.state == ChatState::Starting {
            return Some(if self.loading() {
                "Loading the session".to_owned()
            } else if self.agent.is_empty() {
                "Starting the agent".to_owned()
            } else {
                format!("Starting {}", self.agent)
            });
        }
        let turn = self.going()?;
        // A subagent's tool running says more than the tool that started
        // the subagent, which runs as long as it does.
        let tool = (turn.items.iter().rev())
            .filter_map(Item::tool)
            .find(|tool| tool.status == ToolStatus::Running);
        Some(match (tool, turn.items.last().map(Item::inner)) {
            (Some(tool), _) => tool.doing(),
            (None, Some(Item::Thought(_))) => "Thinking".to_owned(),
            (None, Some(Item::Words(_))) => "Writing".to_owned(),
            _ => "Working".to_owned(),
        })
    }

    /// Whether the agent starting is opening a session the chat had
    /// before, or telling it again: the turn going already has the
    /// agent's work, as one told again does, or a turn before it reached
    /// the agent since the chat last moved to a new session with another.
    fn loading(&self) -> bool {
        if self.state != ChatState::Starting {
            return false;
        }
        let (before, going) = match self.going() {
            Some(turn) => (&self.turns[..self.turns.len() - 1], Some(turn)),
            None => (&self.turns[..], None),
        };
        if going.is_some_and(|turn| !turn.items.is_empty()) {
            return true;
        }
        for turn in before.iter().rev() {
            if let Some(switch) = &turn.switch {
                return switch.resumed;
            }
            // A turn that failed before the agent did anything never
            // reached a session.
            let failed = matches!(
                turn.end,
                Some(TurnEnd {
                    outcome: Outcome::Failed(_),
                    ..
                })
            );
            if !turn.items.is_empty() || (turn.end.is_some() && !failed) {
                return true;
            }
        }
        false
    }
}

impl Tool {
    /// What the line that shows the agent working says while it uses
    /// this tool: "Running: cargo test" for a command, "Reading
    /// Cargo.toml" for "Read Cargo.toml", "Running a command" for a
    /// command titled "Run a command", rather than the verb twice, or its
    /// title as it is; cut short with an ellipsis.
    #[must_use]
    pub fn doing(&self) -> String {
        let first = self.title.lines().next().unwrap_or_default();
        let title: String = first.trim().chars().filter(|&ch| ch != '`').collect();
        let title = match title.char_indices().nth(LONGEST_DOING) {
            Some((at, _)) => format!("{}…", title[..at].trim_end()),
            None => title,
        };
        if title.is_empty() {
            return match self.kind {
                ToolKind::Read => "Reading",
                ToolKind::Edit => "Editing",
                ToolKind::Delete => "Deleting",
                ToolKind::Move => "Moving",
                ToolKind::Search => "Searching",
                ToolKind::Execute => "Running a command",
                ToolKind::Think => "Thinking",
                ToolKind::Fetch => "Fetching",
                ToolKind::Other => "Working",
            }
            .to_owned();
        }
        let (verb, rest) = title.split_once(' ').unwrap_or((&title, ""));
        match DOING_VERBS.iter().find(|(said, _)| *said == verb) {
            Some((_, doing)) if rest.is_empty() => (*doing).to_owned(),
            Some((_, doing)) => format!("{doing} {rest}"),
            // A command's title is the command itself.
            None if self.kind == ToolKind::Execute => format!("Running: {title}"),
            None => title,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Following a task's output is a program and its arguments, never a
    /// command line a shell reads: whatever the path holds is one
    /// argument, after `--`, so it's neither run nor taken for an option
    /// (CLACKA-199).
    #[test]
    fn following_a_tasks_output_runs_tail_with_the_path_as_one_argument_and_no_shell() {
        let path = "/tmp/claude-1/x/tasks/-rf $(touch y); `z` | w.output";
        let task = AgentTask {
            id: "t".into(),
            kind: AgentTaskKind::Shell,
            title: "t".into(),
            command: None,
            state: AgentTaskState::Running,
            summary: None,
            started: 0,
            ended: None,
            can_stop: true,
            output: Some(path.into()),
        };
        assert_eq!(
            task.follow_output("/usr/bin/tail").unwrap(),
            ["/usr/bin/tail", "-c", "262144", "-F", "--", path]
        );
        let none = AgentTask {
            output: None,
            ..task
        };
        assert_eq!(none.follow_output("tail"), None);
    }

    #[test]
    fn a_tasks_line_says_what_its_subagent_does_now_and_how_many_tools_it_used() {
        let tool = |id: &str, title: &str, status| Tool {
            id: id.into(),
            title: title.into(),
            status,
            kind: ToolKind::Read,
            content: Vec::new(),
            locations: Vec::new(),
            exit: None,
        };
        let nested = |item| Item::nested("task".into(), item);
        let mut turn = Turn {
            prompt: "go".into(),
            sent: 0,
            items: vec![
                Item::Tool(tool("task", "Find it", ToolStatus::Running)),
                nested(Item::Tool(tool("r", "Read src/main.rs", ToolStatus::Done))),
                nested(Item::Tool(tool("inner", "Inner", ToolStatus::Running))),
                Item::nested("inner".into(), Item::Words("deeper".into())),
                Item::Words("mine".into()),
            ],
            end: None,
            files: None,
            from: None,
            switch: None,
            previews: Vec::new(),
            woke: None,
        };
        let work = turn.nested_work();
        let task = turn.items[0].tool().unwrap().clone();
        assert_eq!(work["task"].tools, 2);
        assert_eq!(work["inner"].items, 1);
        assert_eq!(
            turn.nested_line(&task, &work["task"]),
            "↳ Inner · 2 tool uses"
        );
        turn.items.push(nested(Item::Words(
            "Found it.\nIn main.rs, at line 4.\n\n".into(),
        )));
        let work = turn.nested_work();
        assert_eq!(
            turn.nested_line(&task, &work["task"]),
            "↳ In main.rs, at line 4. · 2 tool uses"
        );
        let done = Tool {
            status: ToolStatus::Done,
            ..task
        };
        assert_eq!(
            turn.nested_line(&done, &work["task"]),
            "↳ Done · 2 tool uses"
        );
        assert_eq!(turn.nested_line(&done, &work["inner"]), "↳ Done");
        // What changed files and ran commands, its own subagent's too, is
        // said on the card it's folded under, however each went.
        let with = |kind, path: &str, status| Tool {
            kind,
            locations: vec![Location {
                path: path.into(),
                line: None,
            }],
            ..tool(path, path, status)
        };
        turn.items.extend([
            Item::nested(
                "inner".into(),
                Item::Tool(with(ToolKind::Edit, "a.rs", ToolStatus::Done)),
            ),
            nested(Item::Tool(with(ToolKind::Edit, "a.rs", ToolStatus::Done))),
            nested(Item::Tool(with(ToolKind::Delete, "b.rs", ToolStatus::Done))),
            nested(Item::Tool(with(
                ToolKind::Edit,
                "c.rs",
                ToolStatus::Running,
            ))),
            nested(Item::Tool(with(
                ToolKind::Execute,
                "make",
                ToolStatus::Done,
            ))),
            nested(Item::Tool(with(
                ToolKind::Other,
                "mcp__x__y",
                ToolStatus::Failed,
            ))),
        ]);
        let work = turn.nested_work();
        assert_eq!(
            turn.nested_line(&done, &work["task"]),
            "↳ Done · 7 tool uses · edited 3 files · ran 1 command · used 1 other tool"
        );
        assert_eq!(
            turn.nested_line(&done, &work["inner"]),
            "↳ Done · 1 tool use · edited 1 file"
        );
        assert!(
            Turn {
                items: Vec::new(),
                ..turn
            }
            .nested_work()
            .is_empty()
        );
    }

    #[test]
    fn only_the_users_own_words_are_prompts_to_recall() {
        assert_eq!(turn("fix it", Vec::new()).user_prompt(), Some("fix it"));
        assert_eq!(turn("  \n", Vec::new()).user_prompt(), None);
        let from = Turn {
            from: Some(Sender {
                chat: crate::TerminalId(2),
                agent: "Clackatoo".into(),
                title: "Subagents finished".into(),
            }),
            ..turn("they're done", Vec::new())
        };
        assert_eq!(from.user_prompt(), None);
        let woke = Turn {
            woke: Some("the build ended".into()),
            ..turn("", Vec::new())
        };
        assert_eq!(woke.user_prompt(), None);
    }

    #[test]
    fn prompts_kept_one_at_a_time_are_those_worked_out_from_the_turns() {
        // Repeats, a blank, one too long, and enough bytes to let the
        // oldest go.
        let long = "x".repeat(PROMPT_LONGEST + 1);
        let big = |n: usize| format!("{n}{}", "y".repeat(PROMPT_LONGEST - 1));
        let mut said: Vec<String> = vec!["a".into(), "b".into(), "a".into(), " ".into(), long];
        said.extend((0..8).map(big));
        said.extend((0..PROMPTS_KEPT + 20).map(|n| format!("p{}", n % 230)));
        said.push("b".into());
        let turns: Vec<Turn> = said.iter().map(|text| turn(text, Vec::new())).collect();
        let mut kept = Vec::new();
        for turn in &turns {
            if let Some(prompt) = turn.user_prompt() {
                remember_prompt(&mut kept, prompt);
            }
        }
        assert_eq!(kept, recent_prompts(&turns));
        assert_eq!(kept.len(), PROMPTS_KEPT);
        assert_eq!(kept.last().map(String::as_str), Some("b"));
        assert!(kept.iter().map(String::len).sum::<usize>() <= PROMPTS_BYTES);
        // By bytes, before by count.
        let turns: Vec<Turn> = (0..10).map(|n| turn(&big(n), Vec::new())).collect();
        let worked_out = recent_prompts(&turns);
        assert_eq!(worked_out.len(), PROMPTS_BYTES / PROMPT_LONGEST);
        assert!(worked_out[0].starts_with('2'));
    }

    #[test]
    fn a_turn_the_user_sent_is_kept_to_recall_as_it_comes_once() {
        let mut chat = ChatNode {
            turns: vec![turn("one", Vec::new())],
            prompts: vec!["one".into()],
            ..ChatNode::default()
        };
        chat.apply(ChatPatch::Turn {
            index: 1,
            turn: Box::new(turn("two", Vec::new())),
        });
        // Changed after, it's still the one message.
        chat.apply(ChatPatch::Turn {
            index: 1,
            turn: Box::new(turn("two", vec![Item::Words("done".into())])),
        });
        assert_eq!(chat.prompts, ["one", "two"]);
        chat.draft = Some("and".into());
        chat.apply(ChatPatch::DraftCleared { sent: "two".into() });
        assert_eq!(chat.draft.as_deref(), Some(""));
    }

    #[test]
    fn a_slash_command_is_a_word_after_a_slash_at_the_start() {
        assert_eq!(slash_command("/compact"), Some(("compact", "")));
        assert_eq!(
            slash_command("  /review-branch  main \n"),
            Some(("review-branch", "main"))
        );
        assert_eq!(slash_command("/mcp:git:log 3"), Some(("mcp:git:log", "3")));
        for words in ["", "/", "/ x", "a /b", "/etc/hosts is wrong", "//x"] {
            assert_eq!(slash_command(words), None, "{words:?}");
        }
    }

    #[test]
    fn an_interface_command_is_the_agents_if_it_offers_one_of_the_name() {
        assert_eq!(interface_command("/clear", &[]), Some(("clear", "")));
        assert_eq!(interface_command("/Clear ", &[]), Some(("clear", "")));
        assert_eq!(
            interface_command("/help me find why this test fails", &[]),
            Some(("help", "me find why this test fails"))
        );
        assert_eq!(interface_command("/compact", &[]), None);
        let offered = [AgentCommand {
            name: "clear".into(),
            description: String::new(),
            hint: None,
        }];
        assert_eq!(interface_command("/CLEAR", &offered), None);
    }

    #[test]
    fn an_agent_command_is_one_it_offers_or_else_one_its_adapter_knows() {
        assert!(agent_command("/compact", &[]));
        assert!(agent_command("/Review main", &[]));
        assert!(!agent_command("/usr is full, clean it up", &[]));
        let offered = [AgentCommand {
            name: "deploy".into(),
            description: String::new(),
            hint: None,
        }];
        assert!(agent_command("/deploy now", &offered));
        assert!(
            !agent_command("/compact", &offered),
            "it said what it offers"
        );
    }

    #[test]
    fn history_pages_keep_absolute_patches_and_reject_gaps_or_duplicates() {
        let full = ChatNode {
            turns: (0..24)
                .map(|i| turn(&i.to_string(), vec![Item::Words("answer".into())]))
                .collect(),
            ..ChatNode::default()
        };
        let mut recent = full.since_turn(None);
        assert_eq!(
            (recent.first_turn, recent.turns.len(), recent.first_message),
            (16, 8, 16)
        );
        recent.apply(ChatPatch::Append {
            turn: 23,
            item: 0,
            text: " live".into(),
        });
        recent.apply(ChatPatch::Append {
            turn: 1,
            item: 0,
            text: "unloaded".into(),
        });
        let page = ChatPatch::History {
            first: 8,
            before: 16,
            messages_before: 8,
            turns: full.turns[8..16].to_vec(),
        };
        recent.apply(page.clone());
        let once = recent.clone();
        recent.apply(page);
        recent.apply(ChatPatch::History {
            first: 0,
            before: 7,
            messages_before: 0,
            turns: full.turns[..7].to_vec(),
        });
        assert_eq!(recent, once);
        assert_eq!(
            recent.turns.last().unwrap().items,
            [Item::Words("answer live".into())]
        );
        recent.apply(ChatPatch::Turn {
            index: 24,
            turn: Box::new(turn("next", vec![])),
        });
        assert_eq!(recent.turns.last().unwrap().prompt, "next");
    }

    #[test]
    fn history_pages_are_byte_bounded_and_keep_large_turns_whole() {
        let mut full = ChatNode {
            turns: (0..100)
                .map(|_| turn("prompt", vec![Item::Words("x".repeat(160_000))]))
                .collect(),
            ..ChatNode::default()
        };
        let recent = full.since_turn(None);
        assert_eq!(recent.turns.len(), 3);
        assert!(postcard::experimental::serialized_size(&recent).unwrap() < 512 * 1024);
        full.turns.last_mut().unwrap().items = vec![Item::Words("x".repeat(700_000))];
        let recent = full.since_turn(None);
        assert_eq!(recent.turns.len(), 1);
        assert_eq!(recent.turns[0], full.turns[99]);
    }

    #[test]
    fn the_context_used_is_a_percentage_only_of_a_size_said_and_never_past_100() {
        let usage = |used, size| Usage {
            used,
            size,
            five_hour: None,
            weekly: None,
            cost: None,
        };
        assert_eq!(usage(84_000, 200_000).percent(), Some(42));
        assert_eq!(usage(0, 200_000).percent(), Some(0));
        assert_eq!(usage(1, 0).percent(), None);
        assert_eq!(usage(944_000, 200_000).percent(), Some(100));
        assert_eq!(usage(u64::MAX, 1).percent(), Some(100));
        assert_eq!(usage(u64::MAX, u64::MAX).percent(), Some(100));
    }

    #[test]
    fn a_cost_is_said_in_its_currency_to_the_cent_or_below_one() {
        let usd = |amount: f64| Cost::of(amount, "usd").unwrap().label();
        assert_eq!(usd(0.0421), "$0.04");
        assert_eq!(usd(12.345), "$12.35");
        assert_eq!(usd(0.0031), "$0.0031");
        assert_eq!(usd(0.0), "$0.00");
        assert_eq!(Cost::of(1.2, "EUR").unwrap().label(), "1.20 EUR");
        for (amount, currency) in [(-1.0, "USD"), (f64::NAN, "USD"), (1.0, "US"), (1e9, "USD")] {
            assert_eq!(Cost::of(amount, currency), None, "{amount} {currency}");
        }
        // What a turn cost is what the session's grew by, in one currency.
        let at = |amount| Cost::of(amount, "USD");
        assert_eq!(at(0.05).unwrap().since(at(0.02)), at(0.03));
        assert_eq!(at(0.05).unwrap().since(None), at(0.05));
        assert_eq!(at(0.01).unwrap().since(at(0.02)), None);
        assert_eq!(at(0.05).unwrap().since(Cost::of(0.01, "EUR")), None);
    }

    #[test]
    fn what_a_turn_spent_leaves_out_a_cost_of_nothing() {
        let spent = |micros| Spent {
            input: 24_000,
            output: 0,
            cached: 24_000,
            cost: Some(Cost {
                micros,
                currency: *b"USD",
            }),
        };
        assert_eq!(spent(0).label(), "24k tokens");
        assert_eq!(spent(31_000).label(), "24k tokens · $0.03");
    }

    fn usage(used: u64, size: u64) -> Usage {
        Usage {
            used,
            size,
            five_hour: None,
            weekly: None,
            cost: None,
        }
    }

    #[test]
    fn a_meter_is_amber_past_80_and_red_past_95_and_says_tokens_without_a_size() {
        assert_eq!(Fullness::of(0), Fullness::Room);
        assert_eq!(Fullness::of(80), Fullness::Room);
        assert_eq!(Fullness::of(81), Fullness::Filling);
        assert_eq!(Fullness::of(95), Fullness::Filling);
        assert_eq!(Fullness::of(96), Fullness::Full);
        assert_eq!(usage(84_000, 200_000).label(), "42% context");
        assert_eq!(usage(944_000, 200_000).label(), "100% context");
        assert_eq!(usage(84_000, 0).label(), "84k context");
        assert_eq!(tokens(u64::MAX), "18446744073709.5M");
        assert_eq!(tokens(1_250_000), "1.3M");
    }

    #[test]
    fn a_limit_shows_beside_a_meter_only_once_its_getting_full() {
        let limit = |used| Some(Limit { used, resets: None });
        let mut usage = usage(1000, 200_000);
        assert_eq!(usage.pressing(0), None);
        usage.five_hour = limit(42);
        usage.weekly = limit(80);
        assert_eq!(usage.pressing(0), None);
        usage.weekly = limit(91);
        assert_eq!(
            usage.pressing(0),
            Some(("Weekly 91%".into(), Fullness::Filling))
        );
        usage.five_hour = limit(100);
        assert_eq!(
            usage.pressing(0),
            Some(("5-hour 100%".into(), Fullness::Full))
        );
    }

    #[test]
    fn the_detail_says_the_tokens_and_each_limit_with_when_it_resets() {
        let mut usage = usage(84_000, 200_000);
        usage.five_hour = Some(Limit {
            used: 42,
            resets: Some(1),
        });
        usage.weekly = Some(Limit {
            used: 100,
            resets: Some(2),
        });
        let said: Vec<String> = (usage.detail(0, |at| format!("at {at}")).into_iter())
            .map(|line| format!("{}: {} ({:?})", line.name, line.said, line.fullness))
            .collect();
        assert_eq!(
            said,
            [
                "Context: 84k of 200k tokens, 42% (Some(Room))",
                "5-hour limit: 42% used, resets at 1 (Some(Room))",
                "Weekly limit: reached, resets at 2 (Some(Full))",
            ]
        );
        // No size said, a limit with no time said, and none.
        usage.size = 0;
        usage.five_hour = Some(Limit {
            used: 96,
            resets: None,
        });
        usage.weekly = None;
        let lines = usage.detail(0, |_| unreachable!("no time to say"));
        assert_eq!(
            (lines[0].said.as_str(), lines[0].fullness),
            ("84k tokens", None)
        );
        assert_eq!(lines[1].said, "96% used");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn a_limit_past_its_reset_is_no_longer_known() {
        let mut usage = usage(84_000, 200_000);
        usage.five_hour = Some(Limit {
            used: 99,
            resets: Some(1_000),
        });
        usage.weekly = Some(Limit {
            used: 91,
            resets: None,
        });
        let pressing = |now| usage.pressing(now).map(|(said, _)| said);
        assert_eq!(pressing(999).as_deref(), Some("5-hour 99%"));
        // Reset since: how much is used again isn't known till the agent
        // says, and a limit with no time said stays.
        assert_eq!(pressing(1_000).as_deref(), Some("Weekly 91%"));
        let names: Vec<String> = (usage.detail(1_000, |_| String::new()).into_iter())
            .map(|line| line.name)
            .collect();
        assert_eq!(names, ["Context", "Weekly limit"]);
    }

    fn turn(prompt: &str, items: Vec<Item>) -> Turn {
        Turn {
            woke: None,
            previews: Vec::new(),
            prompt: prompt.into(),
            sent: 0,
            items,
            end: None,
            files: None,
            from: None,
            switch: None,
        }
    }

    fn tool(title: &str, kind: ToolKind, status: ToolStatus) -> Item {
        Item::Tool(Tool {
            id: title.into(),
            title: title.into(),
            status,
            kind,
            content: Vec::new(),
            locations: Vec::new(),
            exit: None,
        })
    }

    #[test]
    fn what_the_agent_is_doing_is_said_from_its_latest_work() {
        let mut chat = ChatNode {
            agent: "Claude Code".into(),
            state: ChatState::Starting,
            ..ChatNode::default()
        };
        // Starting, with or without a message waiting for it.
        assert!(chat.working());
        assert_eq!(chat.doing().as_deref(), Some("Starting Claude Code"));
        chat.turns.push(turn("hi", vec![]));
        assert_eq!(chat.doing().as_deref(), Some("Starting Claude Code"));
        // Answering: what it's doing comes from its latest work.
        chat.state = ChatState::Answering;
        let said = |chat: &mut ChatNode, items: Vec<Item>| {
            chat.turns.last_mut().unwrap().items = items;
            chat.doing()
        };
        assert_eq!(said(&mut chat, vec![]).as_deref(), Some("Working"));
        let thought = Item::Thought("hmm".into());
        assert_eq!(
            said(&mut chat, vec![thought.clone()]).as_deref(),
            Some("Thinking")
        );
        let words = Item::Words("It's".into());
        assert_eq!(
            said(&mut chat, vec![thought.clone(), words]).as_deref(),
            Some("Writing")
        );
        let read = tool("Read Cargo.toml", ToolKind::Read, ToolStatus::Running);
        assert_eq!(
            said(&mut chat, vec![read.clone()]).as_deref(),
            Some("Reading Cargo.toml")
        );
        // A tool still running is what it's doing, whatever came after.
        assert_eq!(
            said(&mut chat, vec![read, thought.clone()]).as_deref(),
            Some("Reading Cargo.toml")
        );
        // Between tools.
        let done = tool("Read Cargo.toml", ToolKind::Read, ToolStatus::Done);
        assert_eq!(said(&mut chat, vec![done]).as_deref(), Some("Working"));
        // Asking: it waits for the user, so it isn't working.
        chat.question = Some(Question {
            id: 1,
            title: "Run ls".into(),
            choices: Vec::new(),
            own: false,
            form: None,
            plan: None,
            tool: None,
        });
        assert!(chat.asking() && !chat.working() && chat.doing().is_none());
        chat.question = None;
        assert!(!chat.asking());
        // Done.
        chat.turns.last_mut().unwrap().end = Some(TurnEnd {
            at: 1,
            outcome: Outcome::Done,
            raw: None,
            spent: None,
        });
        chat.state = ChatState::Ready;
        assert!(!chat.working() && chat.doing().is_none());
        // Starting again, with turns from before: it loads the session.
        chat.state = ChatState::Starting;
        assert_eq!(chat.doing().as_deref(), Some("Loading the session"));
        chat.turns.push(turn("more", vec![]));
        assert_eq!(chat.doing().as_deref(), Some("Loading the session"));
        // Not after moving to a new session with another agent.
        chat.turns.insert(1, turn("", vec![]));
        chat.turns[1].end = chat.turns[0].end.clone();
        chat.turns[1].switch = Some(Switch {
            from: "Claude Code".into(),
            to: "Codex".into(),
            model: None,
            resumed: false,
            notes: Vec::new(),
        });
        chat.agent = "Codex".into();
        assert_eq!(chat.doing().as_deref(), Some("Starting Codex"));
        chat.state = ChatState::Failed("no".into());
        assert!(!chat.working() && chat.doing().is_none());
    }

    #[test]
    fn the_daemons_own_suggestion_while_it_answers_doesnt_stop_it_working() {
        let mut chat = ChatNode {
            agent: "Claude Code".into(),
            state: ChatState::Answering,
            turns: vec![turn("hi", vec![])],
            ..ChatNode::default()
        };
        chat.question = Some(Question {
            id: 1,
            title: "Install Claude Code's adapter?".into(),
            choices: Vec::new(),
            own: true,
            form: None,
            plan: None,
            tool: None,
        });
        assert!(chat.working() && !chat.asking());
        assert_eq!(chat.doing().as_deref(), Some("Working"));
        // Its own question while starting, as whether to install, does.
        chat.state = ChatState::Starting;
        assert!(!chat.working() && chat.asking());
        // Ready, with a turn not yet ended: not working.
        chat.question = None;
        chat.state = ChatState::Ready;
        assert!(!chat.working());
    }

    #[test]
    fn only_turns_that_reached_the_agent_mean_a_session_to_load() {
        let mut chat = ChatNode {
            agent: "Claude Code".into(),
            state: ChatState::Starting,
            turns: vec![turn("hi", vec![]), turn("again", vec![])],
            ..ChatNode::default()
        };
        chat.turns[0].sent = 500;
        chat.turns[1].sent = 1_000;
        // The first failed before the agent did anything: no session.
        chat.turns[0].end = Some(TurnEnd {
            at: 600,
            outcome: Outcome::Failed("couldn't start".into()),
            raw: None,
            spent: None,
        });
        assert_eq!(chat.doing().as_deref(), Some("Starting Claude Code"));
        assert_eq!(chat.counted_from(), Some(1_000));
        // One stopped, or done, did.
        chat.turns[0].end = Some(TurnEnd {
            at: 600,
            outcome: Outcome::Stopped,
            raw: None,
            spent: None,
        });
        assert_eq!(chat.doing().as_deref(), Some("Loading the session"));
        assert_eq!(chat.counted_from(), None, "no seconds while it loads");
        // A turn going that already has the agent's work is one told
        // again, as it loads.
        chat.turns.remove(0);
        assert_eq!(chat.doing().as_deref(), Some("Starting Claude Code"));
        chat.turns[0].items.push(Item::Words("Hello".into()));
        assert_eq!(chat.doing().as_deref(), Some("Loading the session"));
        assert_eq!(chat.counted_from(), None);
        // Answering, the seconds count.
        chat.state = ChatState::Answering;
        assert_eq!(chat.counted_from(), Some(1_000));
    }

    #[test]
    fn a_titles_code_is_what_its_backticks_quote_and_nothing_can_end_it_early() {
        let words = |text: &str| Said::Words(text.to_owned());
        let code = |text: &str| Said::Code(text.to_owned());
        let said = |title: &str| {
            Question {
                id: 1,
                title: title.to_owned(),
                choices: Vec::new(),
                own: false,
                form: None,
                plan: None,
                tool: None,
            }
            .said()
        };
        assert_eq!(
            said("run `make` here"),
            [words("run "), code("make"), words(" here")]
        );
        assert_eq!(said("Edit notes.txt"), [words("Edit notes.txt")]);
        // A run with nothing to end it is just backticks.
        assert_eq!(said("a ` b"), [words("a ` b")]);
        assert_eq!(said("``a` b"), [words("``a` b")]);
        // What the daemon quotes comes back whole, whatever's in it.
        for text in [
            "look at the `parser`",
            "x` to chat 1 `y",
            "``",
            " both ",
            "`",
            "a\\",
        ] {
            let title = format!("send {} to chat 2", code_span(text));
            assert_eq!(
                said(&title),
                [words("send "), code(text), words(" to chat 2")],
                "{title}"
            );
        }
    }

    #[test]
    fn a_tool_is_said_as_what_its_doing() {
        let doing = |title: &str, kind: ToolKind| {
            let Item::Tool(tool) = tool(title, kind, ToolStatus::Running) else {
                unreachable!()
            };
            tool.doing()
        };
        assert_eq!(
            doing("cargo test", ToolKind::Execute),
            "Running: cargo test"
        );
        assert_eq!(
            doing("`cargo test`\nand more", ToolKind::Execute),
            "Running: cargo test"
        );
        // A command titled with a verb doesn't say it twice.
        assert_eq!(
            doing("Run a command", ToolKind::Execute),
            "Running a command"
        );
        assert_eq!(
            doing("Read Cargo.toml", ToolKind::Read),
            "Reading Cargo.toml"
        );
        assert_eq!(
            doing("Edit `src/lib.rs`", ToolKind::Edit),
            "Editing src/lib.rs"
        );
        assert_eq!(doing("Fetch", ToolKind::Fetch), "Fetching");
        assert_eq!(doing("grep -r foo", ToolKind::Search), "grep -r foo");
        assert_eq!(doing("", ToolKind::Search), "Searching");
        assert_eq!(doing("  ", ToolKind::Execute), "Running a command");
        // A long one is cut short, and says so.
        let long = doing(&"x".repeat(200), ToolKind::Execute);
        assert_eq!(long.chars().count(), "Running: ".len() + LONGEST_DOING + 1);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn turns_are_numbered_without_those_where_the_chat_moved() {
        let mut moved = turn("", vec![]);
        moved.switch = Some(Switch {
            from: "Claude Code".into(),
            to: "Codex".into(),
            model: None,
            resumed: false,
            notes: Vec::new(),
        });
        let turns = [turn("a", vec![]), moved, turn("b", vec![])];
        let numbers: Vec<Option<usize>> = (0..4).map(|at| turn_number(&turns, at)).collect();
        assert_eq!(numbers, [Some(1), None, Some(2), None]);
        let indices: Vec<Option<usize>> = (0..4).map(|n| turn_numbered(&turns, n)).collect();
        assert_eq!(indices, [None, Some(0), Some(2), None]);
    }

    #[test]
    fn patches_add_turns_and_stream_words() {
        let mut chat = ChatNode::default();
        chat.apply(ChatPatch::Turn {
            index: 0,
            turn: Box::new(turn("hi", vec![Item::Words("Hel".into())])),
        });
        chat.apply(ChatPatch::Append {
            turn: 0,
            item: 0,
            text: "lo".into(),
        });
        // Out of range: ignored.
        chat.apply(ChatPatch::Append {
            turn: 3,
            item: 0,
            text: "x".into(),
        });
        chat.apply(ChatPatch::Turn {
            index: 5,
            turn: Box::new(turn("lost", vec![])),
        });
        assert_eq!(chat.turns, [turn("hi", vec![Item::Words("Hello".into())])]);
        assert_eq!(chat.text(), "> hi\n\nHello");
    }
}
