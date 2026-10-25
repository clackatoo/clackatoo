//! What each screen shows, as [`ui`](crate::ui) trees, from the app's
//! state: the chats and terminals, by workspace, with what needs the user
//! first among them; a chat, as the window shows one: each message, the
//! agent's work folded under one line once it's done, its answer, the
//! cards of the chats it started, and its question, with every choice;
//! and a terminal's header and the row of keys under it (its rows are
//! [`grid`](crate::grid)'s).

use clackatoo_protocol::{
    ChatNode, ChatState, ChatSummary, ChoiceKind, Diff, Fullness, Item, Outcome, Said, SettingKind,
    Subagent, SubagentStatus, TerminalInfo, TerminalStatus, Tool, ToolContent, ToolStatus, Turn,
    TurnFilesState,
};

use crate::action::{Action, RowKey};
use crate::markdown;
use crate::state::{Online, OpenChat, State};
use crate::term::{Fit, OpenTerminal};
use crate::ui::{Element, Node, el};

/// The most lines of a command's output, or of a change to a file, shown:
/// a phone needn't hold all of a long one.
const MOST_LINES: usize = 200;

/// What the device knows of its notifications, for the list to offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notifications {
    /// They're on.
    On,
    /// They can be turned on.
    Off,
    /// The user turned them down in the browser's settings.
    Blocked,
    /// The browser can't, as Safari can't until the app is on the home
    /// screen.
    Unsupported { ios: bool },
}

/// The list of chats, by workspace.
#[must_use]
pub fn home(state: &State, notifications: Notifications, now: u64) -> Element {
    crate::dashboard::home(state, notifications, now)
}

/// What a terminal is called: the title its program set, else what it's
/// running, else its number.
fn terminal_title(terminal: &TerminalInfo) -> String {
    let title = terminal.title.trim();
    if !title.is_empty() {
        return title.to_owned();
    }
    match &terminal.command {
        Some(command) => command.clone(),
        None => format!("Terminal {}", terminal.id),
    }
}

/// A terminal, as a row of the list: what it's called, and what it's
/// running, where, and at what size.
pub(crate) fn terminal_row(terminal: &TerminalInfo) -> Element {
    let mut said = Vec::new();
    match terminal.status {
        TerminalStatus::Running => {
            if let Some(command) = &terminal.command
                && command.trim() != terminal.title.trim()
            {
                said.push(command.clone());
            }
        }
        status => said.push(status.to_string()),
    }
    if let Some(cwd) = &terminal.cwd {
        said.push(cwd.clone());
    }
    said.push(format!("{}×{}", terminal.size.cols(), terminal.size.rows()));
    el("li")
        .class("chat")
        .class("terminal")
        .attr("tabindex", "0")
        .attr("role", "button")
        .action(&Action::OpenTerminal(terminal.id))
        .child(
            el("div")
                .class("top")
                .child(el("span").class("prompt-mark").text("❯"))
                .child(el("span").class("title").text(terminal_title(terminal))),
        )
        .child(el("div").class("meta").text(said.join(" · ")))
}

/// The shown terminal's header: back, what it's called, where it is, its
/// size, and whether it's fitted to the phone, to fit or put back.
#[must_use]
pub fn terminal_header(state: &State, term: &OpenTerminal) -> Element {
    let info = state.terminals.iter().find(|info| info.id == term.id);
    let view = term.view.as_ref();
    let title = (view.map(|view| view.title.trim()))
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .or_else(|| info.map(terminal_title))
        .unwrap_or_else(|| format!("Terminal {}", term.id));
    let mut said = Vec::new();
    if let Some(cwd) = (view.and_then(|view| view.cwd.clone())).or_else(|| info?.cwd.clone()) {
        said.push(cwd);
    }
    if let Some(grid) = term.grid() {
        said.push(format!("{}×{}", grid.size.cols(), grid.size.rows()));
    }
    match state.online {
        Online::Lost(retry) => said.push(format!("offline, trying again in {retry}s")),
        Online::Connecting => said.push("connecting…".to_owned()),
        Online::Open => {}
    }
    let fit = match term.fit {
        Fit::Off => el("button")
            .class("quiet")
            .class("fit")
            .action(&Action::Fit)
            .text("Fit"),
        Fit::Asked(_) => el("button")
            .class("quiet")
            .class("fit")
            .attr("disabled", "")
            .text("Fitting…"),
        Fit::On(_) => el("button")
            .class("fit")
            .class("fitted")
            .action(&Action::Unfit)
            .attr("aria-label", "Fitted to the phone: tap to put back")
            .text("Fitted ✕"),
    };
    el("header")
        .class("bar chat-bar")
        .child(
            el("button")
                .class("back")
                .attr("aria-label", "All agents")
                .action(&Action::Home)
                .text("‹"),
        )
        .child(
            el("div")
                .class("heading")
                .child(el("h1").text(title))
                .child(el("div").class("meta").text(said.join(" · "))),
        )
        .child(fit)
}

/// What's under a terminal: whether keys go, and if not, how many were
/// turned down, then the row of keys the phone's keyboard hasn't.
#[must_use]
pub fn key_row(state: &State, term: &OpenTerminal) -> Element {
    let mut part = el("div").class("keys-part");
    let unreached = !state.keys_go();
    if unreached || term.refused > 0 {
        let mut said = if unreached {
            "Not connected: keys aren't sent"
        } else {
            "Keys aren't sent until the terminal shows"
        }
        .to_owned();
        if term.refused > 0 {
            let plural = if term.refused == 1 { "" } else { "s" };
            said = format!("{said} ({} key{plural} turned down)", term.refused);
        }
        part = part.child(el("p").class("refused").text(said));
    }
    if let Some(note) = &state.note {
        part = part.child(el("p").class("note").text(note.clone()));
    }
    let keys = RowKey::ALL.into_iter().map(|key| {
        el("button")
            .class("key")
            .class_if(key == RowKey::Ctrl && term.ctrl, "on")
            .action(&Action::Key(key))
            .text(key.label())
    });
    part.child(el("div").class("key-row").children(keys))
}

/// What the connection is, as a dot and a word.
pub(crate) fn online(online: Online) -> Element {
    let (class, said) = match online {
        Online::Open => ("up", "Connected".to_owned()),
        Online::Connecting => ("connecting", "Connecting…".to_owned()),
        Online::Lost(retry) => ("lost", format!("Offline: trying again in {retry}s")),
    };
    el("span").class("online").class(class).text(said)
}

/// A chat, as a row of the list.
pub(crate) fn chat_row(chat: &ChatSummary, now: u64) -> Element {
    let (status, label) = if chat.asking {
        ("asking", "Needs approval")
    } else {
        match &chat.state {
            ChatState::Failed(_) => ("error", "Failed"),
            ChatState::Answering | ChatState::Starting => ("running", "Working"),
            ChatState::Ready if chat.unseen => ("unread", "New reply"),
            ChatState::Ready => ("idle", "Ready"),
        }
    };
    let mut side = el("span").class("session-side");
    if chat.active > 0 {
        side = side.child(
            el("span")
                .class("session-time")
                .text(ago(now.saturating_sub(chat.active))),
        );
    }
    if chat.asking {
        side = side.child(el("span").class("badge").text("Approve"));
    } else if chat.unseen {
        side = side.child(
            el("span")
                .class("unread-dot")
                .attr("aria-label", "Unread reply"),
        );
    }
    el("li")
        .class("chat")
        .class(status)
        .class_if(chat.parent.is_some(), "sub")
        .class_if(chat.needs_you(), "needs")
        .attr("tabindex", "0")
        .attr("role", "button")
        .action(&Action::Open(chat.chat))
        .child(
            el("span")
                .class("session-status")
                .class(status)
                .attr("aria-hidden", "true"),
        )
        .child(
            el("span")
                .class("session-body")
                .child(el("span").class("title").text(chat.title.clone()))
                .child(
                    el("span")
                        .class("meta")
                        .child(
                            el("span")
                                .class("session-provider")
                                .text(chat.agent.clone()),
                        )
                        .child(el("span").class("session-state").text(label)),
                ),
        )
        .child(side)
}

/// How long the dots take to pulse one after another and start again, in
/// milliseconds, as the style sheet's `pulse` says.
const DOTS_CYCLE: u64 = 1200;

/// How far behind the one before it each dot pulses, in milliseconds.
const DOTS_LAG: u64 = 200;

/// Three dots that pulse one after another, in the accent colour, while
/// an agent works: the style sheet moves them, and stops them for those
/// who ask for less motion. Each starts as far into its pulse as it would
/// be at `now`, in milliseconds since the Unix epoch, had it been pulsing
/// since the epoch, so dots drawn again, as the list is, carry on rather
/// than start again.
#[must_use]
pub fn dots(now: u64) -> Element {
    let dot = |index: u64| {
        let into = (now + DOTS_CYCLE - DOTS_LAG * index) % DOTS_CYCLE;
        el("i").style("animation-delay", format!("-{into}ms"))
    };
    el("span")
        .class("dots")
        .attr("aria-hidden", "true")
        .children([dot(0), dot(1), dot(2)])
}

/// What a chat is doing, in a word or two.
fn state_word(state: &ChatState) -> &'static str {
    match state {
        ChatState::Starting => "starting",
        ChatState::Ready => "ready",
        ChatState::Answering => "working",
        ChatState::Failed(_) => "failed",
    }
}

/// How long `ms` milliseconds is, roughly: "now", "5m", "3h", "2d".
fn ago(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 => "now".to_owned(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// `ms` milliseconds, short: "<1s", "42s", "4m 58s", "1h 3m".
fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0 => "<1s".to_owned(),
        1..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// The shown chat's header: back, its title, what it's doing, and stop.
/// How much context it fills is by its menus ([`meter`]).
#[must_use]
pub fn chat_header(state: &State, open: &OpenChat) -> Element {
    let node = open.view.as_ref().and_then(|view| view.chat());
    let title = (open.view.as_ref())
        .map(|view| view.title.clone())
        .or_else(|| state.open_summary().map(|chat| chat.title.clone()))
        .unwrap_or_else(|| "Chat".to_owned());
    let mut said = Vec::new();
    if let Some(node) = node {
        said.push(node.agent.clone());
        said.push(state_word(&node.state).to_owned());
    }
    match state.online {
        Online::Lost(retry) => said.push(format!("offline, retrying in {retry}s")),
        Online::Connecting => said.push("reconnecting…".into()),
        Online::Open => {}
    }
    let mut header = el("header")
        .class("bar chat-bar")
        .child(
            el("button")
                .class("back")
                .attr("aria-label", "All agents")
                .action(&Action::Home)
                .text("‹"),
        )
        .child(
            el("div")
                .class("heading")
                .child(el("h1").text(title))
                .child(el("div").class("meta").text(said.join(" · "))),
        );
    if node.is_some_and(|node| node.question.is_some()) {
        header = header.child(
            el("button")
                .class("attention-button")
                .action(&Action::Latest)
                .text("Needs you"),
        );
    }
    if node.is_some_and(|node| node.state == ChatState::Answering) {
        header = header.child(
            el("button")
                .class("stop")
                .action(&Action::Stop)
                .text("Stop"),
        );
    }
    header
}

/// Where turn `index`'s answer starts, among its items: after the last
/// that isn't the agent's words.
fn answer_start(turn: &Turn) -> usize {
    (turn.items.iter())
        .rposition(|item| !matches!(item, Item::Words(_)))
        .map_or(0, |at| at + 1)
}

/// Turn `index` of `node`, the chat shown, as `open` has it folded, at
/// `now`, in milliseconds since the Unix epoch.
#[must_use]
pub fn turn(node: &ChatNode, open: &OpenChat, index: usize, now: u64) -> Element {
    let section = el("section")
        .class("turn")
        .attr("data-turn", (node.first_turn as usize + index).to_string());
    let Some(turn) = node.turns.get(index) else {
        return section;
    };
    let index = node.first_turn as usize + index;
    if let Some(switch) = &turn.switch {
        return section
            .class("switch")
            .child(el("div").class("divider").text(switch.label()));
    }
    let mut section = section;
    if !turn.prompt.is_empty() || !turn.previews.is_empty() {
        let mut prompt = el("div").class("prompt");
        if let Some(from) = &turn.from {
            prompt = prompt.child(el("div").class("from").text(format!("From {}", from.title)));
        }
        if !turn.previews.is_empty() {
            prompt = prompt.child(
                el("div")
                    .class("image-previews")
                    .children(turn.previews.iter().filter(|p| p.valid()).map(preview)),
            );
        }
        if !turn.prompt.is_empty() {
            prompt = prompt.child(el("div").class("bubble").text(turn.prompt.clone()));
        }
        section = section.child(prompt);
    }
    let answer = answer_start(turn);
    let running = turn.end.is_none();
    if answer > 0 || running {
        let key = format!("t{index}");
        let opened = open.folds.get(&key).copied().unwrap_or(running);
        let asking = running && node.asking();
        let mut summary = el("summary").text(fold_label(turn, asking));
        if asking {
            summary = summary.class("waiting");
        }
        let mut work = el("details")
            .class("work")
            .attr("data-fold", key)
            .child(summary);
        if opened {
            work = work.attr("open", "");
        }
        for (at, item) in turn.items[..answer].iter().enumerate() {
            work = work.child(item_view(item, open, index, at));
        }
        section = section.child(work);
    }
    for (at, item) in turn.items.iter().enumerate().skip(answer) {
        section = section.child(item_view(item, open, index, at));
    }
    for subagent in node
        .subagents
        .iter()
        .filter(|subagent| subagent.turn as usize == index)
    {
        section = section.child(card(subagent, now));
    }
    match turn.end.as_ref().map(|end| &end.outcome) {
        Some(Outcome::Failed(why)) => {
            section = section.child(el("p").class("failed").text(why.clone()));
        }
        Some(Outcome::Stopped) => section = section.child(el("p").class("muted").text("Stopped")),
        _ => {}
    }
    if let Some(files) = turn.files {
        let plural = if files.count == 1 { "" } else { "s" };
        let state = match files.state {
            TurnFilesState::Made | TurnFilesState::TooOld => "",
            TurnFilesState::Undone => ", undone",
            TurnFilesState::Partly => ", partly undone",
        };
        section = section.child(
            el("p")
                .class("muted")
                .text(format!("Changed {} file{plural}{state}", files.count)),
        );
    }
    section
}

/// What the line a turn's work folds under says: how long it took, or
/// while it goes, that it's going, or that it waits for you, if it's
/// `asking` you something. What it's doing, and for how long, is said
/// just above the box (see [`working`]), which is always in view and
/// counts the seconds without the turn being drawn again.
fn fold_label(turn: &Turn, asking: bool) -> String {
    let Some(end) = &turn.end else {
        let going = if asking { "Waiting for you" } else { "Working" };
        return going.to_owned();
    };
    if turn.sent == 0 {
        return "Worked".to_owned();
    }
    format!("Worked for {}", duration(end.at.saturating_sub(turn.sent)))
}

/// What the line just above the box says while the agent works, at
/// `now`: what it's doing, "Running: cargo test", and how long the turn
/// going has been going, "· 12s", or nothing if its seconds don't count,
/// as while it starts with no message, or loads the session. `None`
/// while it isn't working, as when it asks you something, or while the
/// app isn't `online`, as what the chat shows may be long over.
#[must_use]
pub fn working(node: &ChatNode, online: Online, now: u64) -> Option<(String, String)> {
    if online != Online::Open {
        return None;
    }
    let doing = node.doing()?;
    let seconds = (node.counted_from())
        .map(|sent| format!("· {}", duration(now.saturating_sub(sent))))
        .unwrap_or_default();
    Some((doing, seconds))
}

/// When the line above the box next counts on a second, after `now`, in
/// milliseconds since the Unix epoch: on the turn's next second, while
/// [`working`] says something with seconds. Otherwise nothing need
/// draw it again.
#[must_use]
pub fn next_second(node: &ChatNode, online: Online, now: u64) -> Option<u64> {
    working(node, online, now)?;
    let sent = node.counted_from()?;
    Some(sent + (now.saturating_sub(sent) / 1000 + 1) * 1000)
}

/// One thing the agent did.
fn item_view(item: &Item, open: &OpenChat, turn: usize, at: usize) -> Element {
    match item {
        Item::Words(words) | Item::Other(words) => {
            el("div").class("words").children(markdown::render(words))
        }
        Item::Thought(thought) => {
            let key = format!("t{turn}i{at}");
            folded(open, key, "thought", el("summary").text("Thinking"))
                .child(el("div").class("words").children(markdown::render(thought)))
        }
        Item::Tool(tool) => tool_view(tool, open, format!("t{turn}i{at}")),
    }
}

/// A part that opens and folds, closed unless opened by hand.
fn folded(open: &OpenChat, key: String, class: &str, summary: Element) -> Element {
    let opened = open.folds.get(&key).copied().unwrap_or(false);
    let details = el("details")
        .class(class)
        .attr("data-fold", key)
        .child(summary);
    if opened {
        details.attr("open", "")
    } else {
        details
    }
}

/// A tool the agent used: its title, and folded under it, what it shows.
fn tool_view(tool: &Tool, open: &OpenChat, key: String) -> Element {
    let mark = match tool.status {
        ToolStatus::Running => "⋯",
        ToolStatus::Done => "✓",
        ToolStatus::Failed => "✗",
    };
    let summary = el("summary")
        .child(el("span").class("mark").text(mark))
        .child(el("span").class("tool-title").text(tool.title.clone()));
    let mut details = folded(open, key, "tool", summary).class(match tool.status {
        ToolStatus::Running => "running",
        ToolStatus::Done => "done",
        ToolStatus::Failed => "failed",
    });
    for content in &tool.content {
        details = details.child(match content {
            ToolContent::Text(text) | ToolContent::Other(text) => {
                el("div").class("words").children(markdown::render(text))
            }
            ToolContent::Diff(diff) => diff_view(diff),
            ToolContent::Output(output) => el("pre").class("output").text(last_lines(output)),
            ToolContent::Image(image) => preview(image),
        });
    }
    if tool.content.is_empty() && !tool.locations.is_empty() {
        let places: Vec<String> = (tool.locations.iter())
            .map(|place| match place.line {
                Some(line) => format!("{}:{line}", place.path),
                None => place.path.clone(),
            })
            .collect();
        details = details.child(el("p").class("muted").text(places.join("\n")));
    }
    details
}

fn preview(image: &clackatoo_protocol::ChatPreview) -> Element {
    el("canvas")
        .class("image-preview")
        .attr("role", "img")
        .attr("aria-label", "Image attachment preview")
        .preview(image)
}

/// The last [`MOST_LINES`] lines of `text`, saying how many went before.
fn last_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MOST_LINES {
        return text.to_owned();
    }
    let cut = lines.len() - MOST_LINES;
    format!("… {cut} lines before\n{}", lines[cut..].join("\n"))
}

/// A change to a file: what went, and what came.
fn diff_view(diff: &Diff) -> Element {
    let mut lines: Vec<Node> = Vec::new();
    let removed = diff
        .old
        .as_deref()
        .unwrap_or_default()
        .lines()
        .map(|line| ("del", '-', line));
    let added = diff.new.lines().map(|line| ("ins", '+', line));
    let all: Vec<_> = removed.chain(added).collect();
    for &(class, sign, line) in all.iter().take(MOST_LINES) {
        lines.push(
            el("span")
                .class(class)
                .text(format!("{sign} {line}\n"))
                .into(),
        );
    }
    if all.len() > MOST_LINES {
        lines.push(Node::Text(format!(
            "… {} lines more\n",
            all.len() - MOST_LINES
        )));
    }
    el("div")
        .class("diff")
        .child(el("div").class("path").text(diff.path.clone()))
        .child(el("pre").children(lines))
}

/// A chat the agent started, as a card in the turn it started in.
fn card(subagent: &Subagent, now: u64) -> Element {
    let status = match &subagent.status {
        SubagentStatus::Starting => "starting".to_owned(),
        SubagentStatus::Working => format!(
            "working · {}",
            duration(now.saturating_sub(subagent.started))
        ),
        SubagentStatus::Asking => "asks you".to_owned(),
        SubagentStatus::Done => match subagent.ended {
            Some(ended) => format!(
                "done in {}",
                duration(ended.saturating_sub(subagent.started))
            ),
            None => "done".to_owned(),
        },
        SubagentStatus::Failed(why) => format!("failed: {why}"),
        SubagentStatus::Closed => "closed".to_owned(),
    };
    let mut who = subagent.agent.clone();
    if let Some(profile) = &subagent.profile {
        who = format!("{profile} · {who}");
    }
    if let Some(model) = &subagent.model {
        who = format!("{who} · {model}");
    }
    el("div")
        .class("card")
        .class_if(subagent.status == SubagentStatus::Asking, "needs")
        .class_if(subagent.status.going(), "going")
        .attr("tabindex", "0")
        .attr("role", "button")
        .action(&Action::Open(subagent.chat))
        .child(el("div").class("task").text(subagent.task.clone()))
        .child(el("div").class("meta").text(format!("{who} · {status}")))
}

/// What's under the shown chat's turns: its notice, and its question,
/// with a button for every choice.
#[must_use]
pub fn footer(state: &State, node: Option<&ChatNode>) -> Element {
    let mut footer = el("div").class("footer");
    if let Some(note) = &state.note {
        footer = footer.child(el("p").class("note").text(note.clone()));
    }
    let Some(node) = node else {
        return footer;
    };
    if let ChatState::Failed(why) = &node.state {
        footer = footer.child(el("p").class("failed").text(why.clone()));
    }
    if let Some(notice) = &node.notice {
        footer = footer.child(el("p").class("notice").text(notice.clone()));
    }
    if let Some(question) = &node.question {
        let choices = question.choices.iter().enumerate().map(|(index, choice)| {
            let kind = match choice.kind {
                ChoiceKind::AllowOnce | ChoiceKind::AllowAlways => "allow",
                ChoiceKind::RejectOnce | ChoiceKind::RejectAlways => "reject",
            };
            el("button")
                .class(kind)
                .action(&Action::Answer {
                    question: question.id,
                    choice: index,
                })
                .text(choice.name.clone())
        });
        // The daemon's own question as it is; any other is about what the
        // agent would do, however it's worded. The code it quotes is code.
        let mut asked = el("p");
        if !question.own {
            asked = asked.child(Node::Text("Allow the agent to ".to_owned()));
        }
        for piece in question.said() {
            asked = asked.child(match piece {
                Said::Words(words) => Node::Text(words),
                Said::Code(code) => el("code").text(code).into(),
            });
        }
        if !question.own {
            asked = asked.child(Node::Text("?".to_owned()));
        }
        footer = footer.child(
            el("div")
                .class("question")
                .child(asked)
                .child(el("div").class("choices").children(choices)),
        );
    }
    footer
}

/// How many minutes ahead of UTC the phone's clock is, or was, or will
/// be, at a time in milliseconds since the Unix epoch.
pub type Offset<'a> = &'a dyn Fn(u64) -> i64;

/// What goes along the top of the box a message is typed in: the menus,
/// with the meter at their end, and its detail above them once tapped, as
/// [`usage_detail`] says.
#[must_use]
pub fn box_bar(node: &ChatNode, open: &OpenChat, now: u64, offset: Offset<'_>) -> Element {
    let model = node
        .settings
        .iter()
        .find(|s| s.kind == SettingKind::Model)
        .map_or_else(
            || node.agent.clone(),
            |s| {
                s.choices
                    .iter()
                    .find(|c| c.value == s.value)
                    .map_or_else(|| s.value.clone(), |c| c.name.clone())
            },
        );
    let mut bar = el("div")
        .class("composer-bar")
        .child(
            el("button")
                .class("model-button")
                .action(&Action::Settings)
                .attr("aria-haspopup", "dialog")
                .attr("id", "settings-trigger")
                .child(crate::provider_icon::mark(&node.agent))
                .child(el("span").class("model-name").text(model))
                .child(el("span").class("muted").text("⌄")),
        )
        .child(
            el("button")
                .class("permissions-button")
                .action(&Action::Permissions)
                .attr("aria-label", "Permissions")
                .attr("aria-haspopup", "dialog")
                .attr("title", "Permissions")
                .child(el("span").class("shield-icon").attr("aria-hidden", "true")),
        )
        .child(meter(node, now).unwrap_or_else(|| {
            el("button")
                .class("meter")
                .action(&Action::Usage)
                .text("Usage")
        }));
    if open.folds.get(SETTINGS_FOLD).copied().unwrap_or(false) {
        bar = bar.child(sheet(
            "Agent settings",
            Action::Settings,
            crate::settings::panel(node, &open.picker),
        ));
    }
    if open.folds.get(USAGE_FOLD).copied().unwrap_or(false) {
        bar = bar.child(sheet(
            "Context & usage",
            Action::Usage,
            usage_panel(node, open, now, offset),
        ));
    }
    bar
}

fn usage_panel(node: &ChatNode, open: &OpenChat, now: u64, offset: Offset<'_>) -> Element {
    let mut detail = el("div").class("usage-content");
    if let Some(usage) = node.usage {
        detail = detail.child(
            el("section")
                .class("context-card")
                .child(el("h3").text("This chat"))
                .child(
                    el("strong").class("context-total").text(
                        usage
                            .percent()
                            .map_or_else(|| "—".into(), |p| format!("{p}%")),
                    ),
                )
                .child(el("span").class("muted").text(" context used"))
                .children(usage_detail(node, open, now, offset)),
        );
    }
    detail = detail.child(
        el("h3")
            .class("allowances-heading")
            .text("Account allowances"),
    );
    for provider in &node.provider_usage {
        let mut section = el("section").class("provider-usage").child(
            el("h3")
                .child(crate::provider_icon::mark(&provider.agent))
                .text(provider.title.clone()),
        );
        if provider.refreshing {
            section = section.child(el("p").class("muted").text("Refreshing…"));
        }
        if let Some(notice) = &provider.notice {
            section = section.child(el("p").class("muted").text(notice.clone()));
        }
        for limit in &provider.limits {
            let left = 100_u8.saturating_sub(limit.used);
            let mut card = el("div")
                .class("quota-card")
                .class_if(left < 20, "low")
                .child(
                    el("div")
                        .class("quota-heading")
                        .child(el("span").text(limit.label.clone()))
                        .child(el("strong").text(format!("{left}% left"))),
                )
                .child(
                    el("div")
                        .class("quota-track")
                        .attr("role", "meter")
                        .attr("aria-label", limit.label.clone())
                        .attr("aria-valuemin", "0")
                        .attr("aria-valuemax", "100")
                        .attr("aria-valuenow", left.to_string())
                        .child(el("i").style("width", format!("{left}%"))),
                );
            if let Some(at) = limit.resets {
                card = card.child(
                    el("p")
                        .class("muted")
                        .text(format!("Resets {}", resets(at, now, offset))),
                );
            }
            section = section.child(card);
        }
        detail = detail.child(section);
    }
    if node.usage.is_none() && node.provider_usage.is_empty() {
        detail = detail.child(
            el("p")
                .class("muted")
                .text("Usage hasn't been reported yet."),
        );
    }
    detail
}

pub const SETTINGS_FOLD: &str = "settings";

/// The same accessible sheet for settings and account usage. Its close button
/// takes focus; the app contains Tab and restores focus when it closes.
fn sheet(title: &str, close: Action, content: Element) -> Element {
    el("div")
        .class("sheet-layer")
        .child(
            el("button")
                .class("sheet-backdrop")
                .attr("tabindex", "-1")
                .attr("aria-label", "Close panel")
                .action(&close),
        )
        .child(
            el("section")
                .class("sheet")
                .attr("role", "dialog")
                .attr("aria-modal", "true")
                .attr("aria-label", title)
                .child(
                    el("header")
                        .class("sheet-header")
                        .child(el("h2").text(title))
                        .child(
                            el("button")
                                .class("quiet sheet-close")
                                .attr("aria-label", "Close panel")
                                .action(&close)
                                .text("Done"),
                        ),
                )
                .child(el("div").class("sheet-body").child(content)),
        )
}

/// Shared daemon queue, with stable IDs so stale taps cannot affect another item.
#[must_use]
pub fn queue(node: &ChatNode, online: Online) -> Element {
    let mut list = el("div").class("queue");
    if node.delivery.queued.is_empty() {
        return list;
    }
    let title = if node.delivery.paused {
        "Queue paused"
    } else {
        "Up next"
    };
    list = list.child(
        el("div")
            .class("queue-heading")
            .text(format!("{title} · {}", node.delivery.queued.len())),
    );
    for message in &node.delivery.queued {
        let steering =
            node.delivery.native_steering && message.images == 0 && message.target.is_none();
        let busy = matches!(node.state, ChatState::Answering | ChatState::Starting);
        let label = if !busy {
            "Send now"
        } else if steering {
            "Steer now"
        } else {
            "Stop & continue"
        };
        let mut send = el("button")
            .class("quiet")
            .action(&Action::SendQueued(message.id))
            .text(label);
        let mut remove = el("button")
            .class("quiet")
            .attr("aria-label", "Remove queued message")
            .action(&Action::RemoveQueued(message.id))
            .text("×");
        if online != Online::Open || node.delivery.sending.is_some() {
            send = send.attr("disabled", "");
            remove = remove.attr("disabled", "");
        }
        let mut text = el("div").class("queued-text").text(message.text.clone());
        if message.images > 0 {
            text = text.child(el("small").text(format!(" · {} image(s)", message.images)));
        }
        list = list.child(
            el("div")
                .class("queued-message")
                .child(text)
                .child(send)
                .child(remove),
        );
    }
    list
}

/// The fold that opens the meter's detail, in [`OpenChat::folds`].
pub const USAGE_FOLD: &str = "usage";

/// How full something `fullness` full is, as a class: none while there's
/// room.
fn fullness_class(fullness: Fullness) -> Option<&'static str> {
    match fullness {
        Fullness::Room => None,
        Fullness::Filling => Some("filling"),
        Fullness::Full => Some("full"),
    }
}

/// The meter of how much of its context the chat fills, at the end of its
/// menus, as the window has it by the model menu: "42%" over a
/// thin bar, amber past 80% and red past 95%, and before it, the
/// account's limit over five hours or a week, once that's as full, and
/// not yet past when it resets, at `now`. Tapping it shows the detail
/// ([`usage_detail`]), or hides it.
#[must_use]
pub fn meter(node: &ChatNode, now: u64) -> Option<Element> {
    let usage = node.usage?;
    let percent = usage.percent();
    let mut meter = el("button")
        .class("meter")
        .action(&Action::Usage)
        .attr(
            "aria-label",
            format!("{} — context and usage limits", usage.label()),
        )
        .attr("aria-haspopup", "dialog");
    if let Some(class) = percent.map(Fullness::of).and_then(fullness_class) {
        meter = meter.class(class);
    }
    if let Some((said, fullness)) = usage.pressing(now) {
        let limit = el("span").class("limit").text(said);
        meter = meter.child(match fullness_class(fullness) {
            Some(class) => limit.class(class),
            None => limit,
        });
    }
    let mut context = el("span")
        .class("context")
        .child(el("span").text(percent.map_or_else(|| "—".into(), |p| format!("{p}%"))));
    if let Some(percent) = percent {
        let filled = el("i").style("width", format!("{percent}%"));
        context = context.child(el("span").class("bar").child(filled));
    }
    if let Some(percent) = percent {
        meter = meter.style(
            "--context",
            format!("{}deg", u16::from(percent) * 360 / 100),
        );
    }
    Some(meter.child(context))
}

/// The meter's detail, above the chat's menus, once the meter's tapped:
/// the tokens, and each limit of the account's the agent has said, how
/// much of it is used and when it resets, as said at `now`, in
/// milliseconds since the Unix epoch, in the phone's time as `offset`
/// says it.
#[must_use]
pub fn usage_detail(
    node: &ChatNode,
    open: &OpenChat,
    now: u64,
    offset: Offset<'_>,
) -> Option<Element> {
    let usage = node.usage?;
    if !open.folds.get(USAGE_FOLD).copied().unwrap_or(false) {
        return None;
    }
    let rows = (usage.detail(now, |at| resets(at, now, offset)).into_iter()).map(|line| {
        let said = el("dd").text(line.said);
        let said = match line.fullness.and_then(fullness_class) {
            Some(class) => said.class(class),
            None => said,
        };
        [el("dt").text(line.name), said]
    });
    Some(el("dl").class("usage").children(rows.flatten()))
}

/// The days of the week, from Sunday.
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// The months, short.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// When a limit resets, `at`, as said at `now`, both in milliseconds since
/// the Unix epoch, in the phone's time as `offset` says it then, so a
/// change of the clocks between comes out right, as the window says it:
/// "at" and the time today, "tomorrow" and the time, the day of the week
/// and the time within the week, and the date and the time further off,
/// with the year if it's another.
#[must_use]
pub fn resets(at: u64, now: u64, offset: Offset<'_>) -> String {
    const DAY: i64 = 86_400_000;
    let local = |ms: u64| {
        i64::try_from(ms)
            .ok()?
            .checked_add(offset(ms).checked_mul(60_000)?)
    };
    let (Some(at), Some(now)) = (local(at), local(now)) else {
        return String::new();
    };
    let (day, today) = (at.div_euclid(DAY), now.div_euclid(DAY));
    let minutes = at.rem_euclid(DAY) / 60_000;
    let time = format!("{:02}:{:02}", minutes / 60, minutes % 60);
    match day - today {
        0 => format!("at {time}"),
        1 => format!("tomorrow {time}"),
        // The epoch's first day was a Thursday.
        2..=6 => {
            let weekday = usize::try_from((day + 4).rem_euclid(7)).unwrap_or(0);
            format!("{} {time}", WEEKDAYS[weekday])
        }
        _ => {
            let ((year, month, date), (this_year, ..)) = (civil(day), civil(today));
            let month = MONTHS[usize::try_from(month - 1).unwrap_or(0) % 12];
            if year == this_year {
                format!("{date} {month} {time}")
            } else {
                format!("{date} {month} {year} {time}")
            }
        }
    }
}

/// The year, month (from 1) and day of the month `days` days after the
/// Unix epoch, in the proleptic Gregorian calendar (Howard Hinnant's
/// `civil_from_days`).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, date)
}

/// The pairing screen's parts.
pub mod pairing {
    use crate::action::Action;
    use crate::ui::{Element, el};

    /// What the daemon said a device can pair with.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Ways {
        pub password: bool,
        pub ask: bool,
    }

    /// The first screen of an unpaired device: on an iPhone's Safari, to
    /// add the app to the home screen first, as what pairing stores in
    /// Safari the app on the home screen doesn't see; else, how to pair.
    #[must_use]
    pub fn start(ways: Ways, name: &str, add_first: bool) -> Element {
        let mut page = el("main").class("pair").child(el("h1").text("clackatoo"));
        if add_first {
            return page
                .child(el("p").text(
                    "Add clackatoo to your home screen first: tap Share, then Add to Home \
                     Screen. Then open it from there, and pair it.",
                ))
                .child(el("p").class("muted").text(
                    "What pairing here stores, the app on the home screen can't see, and \
                     only that app can be sent notifications.",
                ))
                .child(
                    el("button")
                        .class("quiet")
                        .action(&Action::PairHere)
                        .text("Pair in Safari anyway"),
                );
        }
        page = page
            .child(el("p").text("Pair this device to control clackatoo from it."))
            .child(
                el("label")
                    .child(el("span").text("This device's name"))
                    .child(
                        el("input")
                            .attr("id", "device-name")
                            .attr("value", name)
                            .attr("maxlength", "40"),
                    ),
            );
        if ways.password {
            page = page
                .child(
                    el("label").child(el("span").text("Password")).child(
                        el("input")
                            .attr("id", "password")
                            .attr("type", "password")
                            .attr("autocomplete", "current-password"),
                    ),
                )
                .child(el("button").action(&Action::Pair).text("Pair"));
        }
        if ways.ask {
            page = page
                .child(el("p").class("muted").text(if ways.password {
                    "Or ask a clackatoo window to allow it:"
                } else {
                    "A clackatoo window allows it:"
                }))
                .child(
                    el("button")
                        .class_if(ways.password, "quiet")
                        .action(&Action::Ask)
                        .text("Ask a window"),
                );
        }
        if !ways.password && !ways.ask {
            page = page.child(el("p").text(
                "This daemon lets no device pair: set a password with `clackatoo web password`, \
                 or allow asking in daemon/web.toml.",
            ));
        }
        page
    }

    /// While a window is asked: the code to type there.
    #[must_use]
    pub fn asking(code: &str) -> Element {
        el("main")
            .class("pair")
            .child(el("h1").text("clackatoo"))
            .child(el("p").text("In a clackatoo window, type"))
            .child(el("p").class("code").text(format!(":Devices allow {code}")))
            .child(
                el("p")
                    .class("muted")
                    .text("Waiting for the window, for up to two minutes…"),
            )
    }

    /// The password asked for again.
    #[must_use]
    pub fn renew() -> Element {
        el("main")
            .class("pair")
            .child(el("h1").text("clackatoo"))
            .child(el("p").text("Enter the password again to carry on."))
            .child(
                el("label").child(el("span").text("Password")).child(
                    el("input")
                        .attr("id", "password")
                        .attr("type", "password")
                        .attr("autocomplete", "current-password"),
                ),
            )
            .child(el("button").action(&Action::Renew).text("Carry on"))
    }

    /// Something went wrong, or was turned down: what, and to try again.
    #[must_use]
    pub fn failed(why: &str) -> Element {
        el("main")
            .class("pair")
            .child(el("h1").text("clackatoo"))
            .child(el("p").class("failed").text(why.to_owned()))
            .child(el("button").action(&Action::Again).text("Try again"))
    }

    /// The app isn't let in at all, and why: nothing to try.
    #[must_use]
    pub fn refused(why: &str) -> Element {
        el("main")
            .class("pair")
            .child(el("h1").text("clackatoo"))
            .child(el("p").class("failed").text(why.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{
        Choice, Location, Question, TerminalId, TurnEnd, Workspace, WorkspaceId, Workspaces,
    };

    use super::*;

    fn summary(chat: u32, workspace: Option<u32>, asking: bool, active: u64) -> ChatSummary {
        ChatSummary {
            chat: TerminalId(chat),
            title: format!("chat {chat}"),
            agent: "Claude Code".into(),
            state: ChatState::Ready,
            asking,
            unseen: false,
            parent: None,
            workspace: workspace.map(WorkspaceId),
            active,
        }
    }

    fn open() -> OpenChat {
        let mut state = State::default();
        state.open_chat(TerminalId(1));
        state.open.unwrap()
    }

    #[test]
    fn chats_are_listed_by_workspace_what_needs_you_first() {
        let mut state = State::default();
        state.workspaces = Workspaces {
            home: None,
            restored: Vec::new(),
            list: vec![Workspace {
                id: WorkspaceId(1),
                name: "clackatoo".into(),
                tabs: Vec::new(),
                active_tab: clackatoo_protocol::TabId(1),
                home: None,
                lent: None,
                background: Vec::new(),
            }],
            active: None,
            machine: None,
            others: Vec::new(),
            passed: Vec::new(),
            kept: Vec::new(),
        };
        state.chats = vec![
            summary(1, Some(1), false, 5 * 60_000),
            summary(2, Some(1), true, 0),
            summary(3, None, false, 0),
        ];
        let page = home(&state, Notifications::Off, 10 * 60_000);
        let rows = page.find_all("data-action");
        let opened: Vec<&str> = rows
            .iter()
            .filter_map(|row| row.get("data-action"))
            .collect();
        assert_eq!(
            opened,
            [
                "filter:agents",
                "filter:attention",
                "filter:working",
                "filter:terminals",
                "open:2",
                "open:1",
                "open:3",
                "notify"
            ]
        );
        let text = page.all_text();
        assert!(text.contains("clackatoo"));
        assert!(text.contains("Needs approval"));
        assert!(text.contains("Elsewhere"));
        assert!(text.contains("Claude CodeReady5m"), "{text}");
    }

    #[test]
    fn a_finished_turns_work_folds_under_how_long_it_took() {
        let tool = Tool {
            id: "t".into(),
            title: "Read Cargo.toml".into(),
            status: ToolStatus::Done,
            kind: clackatoo_protocol::ToolKind::Read,
            content: vec![ToolContent::Output("line\n".repeat(300))],
            locations: vec![Location {
                path: "Cargo.toml".into(),
                line: None,
            }],
        };
        let mut node = ChatNode::default();
        node.turns.push(Turn {
            previews: Vec::new(),
            prompt: "look".into(),
            sent: 1_000,
            items: vec![
                Item::Words("Let me look.".into()),
                Item::Tool(tool),
                Item::Words("It's **fine**.".into()),
            ],
            end: Some(TurnEnd {
                at: 43_000,
                outcome: Outcome::Done,
            }),
            files: None,
            from: None,
            switch: None,
        });
        let mut open = open();
        let section = turn(&node, &open, 0, 50_000);
        let work = &section.find_all("data-fold")[0];
        assert_eq!(work.tag, "details");
        assert_eq!(work.get("open"), None, "folded once done");
        assert!(work.all_text().starts_with("Worked for 42s"));
        assert!(work.all_text().contains("Let me look."));
        assert!(work.all_text().contains("… 100 lines before"));
        assert!(section.all_text().ends_with("It's fine."));
        // Opened by hand, it stays open.
        open.folds.insert("t0".into(), true);
        let section = turn(&node, &open, 0, 50_000);
        assert_eq!(section.find_all("data-fold")[0].get("open"), Some(""));
    }

    #[test]
    fn only_the_daemons_own_question_is_shown_as_it_is() {
        let own = "Keep waiting for Codex, start it again and ask again, or stop?";
        let shown = |own_question| {
            let node = ChatNode {
                question: Some(Question {
                    id: 1,
                    title: own.into(),
                    choices: Vec::new(),
                    own: own_question,
                }),
                ..ChatNode::default()
            };
            footer(&State::default(), Some(&node)).all_text()
        };
        assert!(shown(true).contains(own));
        assert!(!shown(true).contains("Allow the agent"));
        // Worded as the daemon's, an agent's is still the agent's.
        assert!(shown(false).contains(&format!("Allow the agent to {own}?")));
    }

    #[test]
    fn the_code_a_question_quotes_is_code() {
        let node = ChatNode {
            question: Some(Question {
                id: 1,
                title: "send `` look at the `parser` `` to a new chat".into(),
                choices: Vec::new(),
                own: false,
            }),
            ..ChatNode::default()
        };
        let footer = footer(&State::default(), Some(&node));
        assert_eq!(
            footer.all_text(),
            "Allow the agent to send look at the `parser` to a new chat?"
        );
        let question = (footer.find_all("class").into_iter())
            .find(|element| element.get("class") == Some("question"))
            .expect("the question");
        let Some(Node::Element(asked)) = question.children.first() else {
            panic!("{question:?}");
        };
        let codes: Vec<String> = (asked.children.iter())
            .filter_map(|child| match child {
                Node::Element(code) if code.tag == "code" => Some(code.all_text()),
                _ => None,
            })
            .collect();
        assert_eq!(codes, ["look at the `parser`"]);
    }

    #[test]
    fn a_question_offers_every_choice() {
        let node = ChatNode {
            question: Some(Question {
                id: 9,
                title: "Write notes.txt".into(),
                choices: ["Allow", "Always allow", "Reject"]
                    .into_iter()
                    .zip([
                        ChoiceKind::AllowOnce,
                        ChoiceKind::AllowAlways,
                        ChoiceKind::RejectOnce,
                    ])
                    .map(|(name, kind)| Choice {
                        id: name.to_lowercase(),
                        name: name.into(),
                        kind,
                    })
                    .collect(),
                own: false,
            }),
            ..ChatNode::default()
        };
        let footer = footer(&State::default(), Some(&node));
        let buttons: Vec<(&str, &str)> = (footer.find_all("data-action").into_iter())
            .map(|button| {
                (
                    button.get("data-action").unwrap(),
                    button.get("class").unwrap(),
                )
            })
            .collect();
        assert_eq!(
            buttons,
            [
                ("answer:9:0", "allow"),
                ("answer:9:1", "allow"),
                ("answer:9:2", "reject")
            ]
        );
    }

    /// A chat whose agent has said it fills `used` of 200k tokens, and
    /// `five_hour` of its account's limit over five hours.
    fn using(used: u64, five_hour: Option<u8>) -> ChatNode {
        ChatNode {
            usage: Some(clackatoo_protocol::Usage {
                used,
                size: 200_000,
                five_hour: five_hour.map(|used| clackatoo_protocol::Limit {
                    used,
                    resets: Some(1_760_543_400_000),
                }),
                weekly: None,
            }),
            ..ChatNode::default()
        }
    }

    #[test]
    fn the_meter_says_the_context_used_quietly_then_amber_then_red() {
        let now = 1_760_529_600_000;
        assert_eq!(meter(&ChatNode::default(), now), None, "nothing said yet");
        let quiet = meter(&using(84_000, Some(42)), now).unwrap();
        assert_eq!(quiet.get("class"), Some("meter"));
        assert_eq!(quiet.get("data-action"), Some("usage"));
        assert_eq!(quiet.all_text(), "42%");
        let bar = &quiet.find_all("class").last().unwrap().children[0];
        assert!(
            matches!(bar, Node::Element(filled) if filled.styles == [("width", "42%".to_owned())]),
            "{bar:?}"
        );
        let filling = meter(&using(170_000, None), now).unwrap();
        assert_eq!(filling.get("class"), Some("meter filling"));
        // Said to fill more than it holds: all of it.
        let full = meter(&using(u64::MAX, None), now).unwrap();
        assert_eq!(full.get("class"), Some("meter full"));
        assert_eq!(full.all_text(), "100%");
        // A limit all but reached goes before it, in red.
        let limit = meter(&using(84_000, Some(97)), now).unwrap();
        assert_eq!(limit.get("class"), Some("meter"));
        assert_eq!(limit.all_text(), "5-hour 97%42%");
    }

    #[test]
    fn the_meters_detail_shows_once_tapped_saying_when_each_limit_resets() {
        let node = using(84_000, Some(42));
        let mut open = open();
        // Wednesday 15 October 2025, 12:00 UTC.
        let now = 1_760_529_600_000;
        assert_eq!(usage_detail(&node, &open, now, &|_| 60), None, "folded");
        open.folds.insert(USAGE_FOLD.into(), true);
        let detail = usage_detail(&node, &open, now, &|_| 60).unwrap();
        // An hour ahead of UTC, as in London in October.
        assert_eq!(
            detail.all_text(),
            "Context84k of 200k tokens, 42%5-hour limit42% used, resets at 16:50"
        );
        assert_eq!(usage_detail(&ChatNode::default(), &open, now, &|_| 0), None);
        // Past when it resets: how much is used now isn't known.
        let later = 1_760_543_400_000;
        let detail = usage_detail(&node, &open, later, &|_| 60).unwrap();
        assert_eq!(detail.all_text(), "Context84k of 200k tokens, 42%");
        let limit = meter(&using(84_000, Some(97)), now).unwrap();
        assert_eq!(limit.all_text(), "5-hour 97%42%");
        let reset = meter(&using(84_000, Some(97)), later).unwrap();
        assert_eq!(reset.all_text(), "42%");
    }

    #[test]
    fn a_reset_is_said_as_the_window_says_it_in_the_phones_time() {
        let now = 1_760_529_600_000;
        let (hour, day) = (3_600_000, 86_400_000);
        let utc = &|_| 0;
        assert_eq!(resets(now + 3 * hour + 2_400_000, now, utc), "at 15:40");
        assert_eq!(resets(now + 13 * hour, now, utc), "tomorrow 01:00");
        // Behind UTC, that's still today.
        assert_eq!(resets(now + 13 * hour, now, &|_| -300), "at 20:00");
        assert_eq!(resets(now + 6 * day - 3 * hour, now, utc), "Tuesday 09:00");
        assert_eq!(resets(now + 20 * day, now, utc), "4 Nov 12:00");
        assert_eq!(resets(now + 120 * day, now, utc), "12 Feb 2026 12:00");
        assert_eq!(resets(u64::MAX, now, utc), "");
        // In London: summer time now, and the clocks gone back by the
        // reset, a fortnight on at 09:00 GMT, which is 09:00 there.
        let summer_till = 1_761_440_400_000;
        let london = &|ms| if ms < summer_till { 60 } else { 0 };
        let reset = now + 14 * day - 3 * hour;
        assert_eq!(resets(reset, now, london), "29 Oct 09:00");
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(-1), (1969, 12, 31));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn terminals_are_listed_beside_the_chats_saying_what_and_where() {
        let mut state = State::default();
        state.workspaces = Workspaces {
            home: None,
            restored: Vec::new(),
            list: vec![Workspace {
                id: WorkspaceId(1),
                name: "clackatoo".into(),
                tabs: vec![clackatoo_protocol::Tab {
                    id: clackatoo_protocol::TabId(1),
                    layout: clackatoo_protocol::Layout::Pane(clackatoo_protocol::Pane {
                        id: clackatoo_protocol::PaneId(1),
                        content: clackatoo_protocol::Content::Terminal(TerminalId(2)),
                        home: None,
                        key: None,
                    }),
                    focused: clackatoo_protocol::PaneId(1),
                    name: None,
                }],
                active_tab: clackatoo_protocol::TabId(1),
                home: None,
                lent: None,
                background: Vec::new(),
            }],
            active: None,
            machine: None,
            others: Vec::new(),
            passed: Vec::new(),
            kept: Vec::new(),
        };
        state.terminals = vec![TerminalInfo {
            id: TerminalId(2),
            pid: Some(1),
            size: clackatoo_protocol::Size::new(80, 24).unwrap(),
            title: String::new(),
            status: TerminalStatus::Running,
            cwd: Some("/home/jack/clackatoo".into()),
            command: Some("cargo test".into()),
        }];
        state.online = Online::Open;
        state.home_filter = crate::state::HomeFilter::Terminals;
        let page = home(&state, Notifications::On, 0);
        let opened: Vec<&str> = (page.find_all("data-action").into_iter())
            .filter_map(|row| row.get("data-action"))
            .collect();
        assert_eq!(
            opened,
            [
                "filter:agents",
                "filter:attention",
                "filter:working",
                "filter:terminals",
                "term:2"
            ]
        );
        let text = page.all_text();
        assert!(text.contains("cargo test"), "{text}");
        assert!(text.contains("/home/jack/clackatoo · 80×24"), "{text}");
    }

    #[test]
    fn a_terminals_header_offers_fitting_and_says_when_its_fitted() {
        let mut state = State::default();
        state.open_terminal(TerminalId(2));
        let size = clackatoo_protocol::Size::new(40, 30).unwrap();
        let term = state.term.as_mut().unwrap();
        let header = terminal_header(&State::default(), term);
        assert_eq!(
            header.find_all("data-action")[1].get("data-action"),
            Some("fit")
        );
        term.fit = Fit::On(size);
        let header = terminal_header(&State::default(), term);
        let fitted = header.find_all("data-action")[1];
        assert_eq!(fitted.get("data-action"), Some("unfit"));
        assert!(fitted.all_text().contains("Fitted"));
    }

    #[test]
    fn the_key_row_says_when_keys_dont_go_and_shows_ctrl_held() {
        let mut state = State::default();
        state.open_terminal(TerminalId(2));
        state.tap_ctrl();
        let term = state.term.as_ref().unwrap();
        let row = key_row(&state, term);
        assert!(
            row.all_text()
                .starts_with("Not connected: keys aren't sent")
        );
        let ctrl = (row.find_all("data-action").into_iter())
            .find(|key| key.get("data-action") == Some("key:ctrl"))
            .unwrap();
        assert_eq!(ctrl.get("class"), Some("key on"));
        let labels: Vec<String> = (row.find_all("data-action").into_iter())
            .map(Element::all_text)
            .collect();
        assert_eq!(
            labels,
            [
                "Esc", "Tab", "Ctrl", "^C", "←", "↑", "↓", "→", "|", "~", "/", "-", "Paste"
            ]
        );
    }

    #[test]
    fn while_the_agent_works_the_line_above_the_box_says_what_and_how_long() {
        let tool = Tool {
            id: "t".into(),
            title: "cargo test".into(),
            status: ToolStatus::Running,
            kind: clackatoo_protocol::ToolKind::Execute,
            content: Vec::new(),
            locations: Vec::new(),
        };
        let mut node = ChatNode {
            agent: "Claude Code".into(),
            state: ChatState::Answering,
            ..ChatNode::default()
        };
        node.turns.push(Turn {
            previews: Vec::new(),
            prompt: "test it".into(),
            sent: 1_000,
            items: vec![Item::Tool(tool)],
            end: None,
            files: None,
            from: None,
            switch: None,
        });
        let said = |node: &ChatNode, now| {
            working(node, Online::Open, now).map(|(doing, seconds)| format!("{doing} {seconds}"))
        };
        assert_eq!(
            said(&node, 13_400).as_deref(),
            Some("Running: cargo test · 12s")
        );
        // Drawn again on the turn's next second.
        assert_eq!(next_second(&node, Online::Open, 13_400), Some(14_000));
        // Out of reach of the daemon: nothing said, nothing counted.
        for online in [Online::Connecting, Online::Lost(3)] {
            assert_eq!(working(&node, online, 13_400), None);
            assert_eq!(next_second(&node, online, 13_400), None);
        }
        // The turn's own line says only that it's going, so it needn't be
        // drawn again as the seconds count.
        let section = turn(&node, &open(), 0, 13_400);
        let work = &section.find_all("data-fold")[0];
        assert!(
            work.all_text().starts_with("Working"),
            "{}",
            work.all_text()
        );
        assert_eq!(turn(&node, &open(), 0, 99_000), section);
        // Thinking, then writing.
        node.turns[0].items = vec![Item::Thought("hmm".into())];
        assert_eq!(said(&node, 13_400).as_deref(), Some("Thinking · 12s"));
        node.turns[0].items.push(Item::Words("It's".into()));
        assert_eq!(said(&node, 13_400).as_deref(), Some("Writing · 12s"));
        // Asking you, it waits: nothing.
        node.question = Some(Question {
            id: 1,
            title: "run ls".into(),
            choices: Vec::new(),
            own: false,
        });
        assert_eq!(said(&node, 13_400), None);
        // The turn's own line says so instead, in the accent colour.
        let section = turn(&node, &open(), 0, 13_400);
        let work = &section.find_all("data-fold")[0];
        assert!(
            work.all_text().starts_with("Waiting for you"),
            "{}",
            work.all_text()
        );
        let waiting = |section: &Element| {
            (section.find_all("class").into_iter())
                .filter(|element| element.get("class") == Some("waiting"))
                .count()
        };
        assert_eq!(waiting(&section), 1, "{section:?}");
        node.question = None;
        let section = turn(&node, &open(), 0, 13_400);
        assert!(
            section.find_all("data-fold")[0]
                .all_text()
                .starts_with("Working")
        );
        assert_eq!(waiting(&section), 0);
        // Starting, before any message: no seconds.
        let starting = ChatNode {
            agent: "Claude Code".into(),
            ..ChatNode::default()
        };
        assert_eq!(
            working(&starting, Online::Open, 0),
            Some(("Starting Claude Code".to_owned(), String::new()))
        );
        assert_eq!(next_second(&starting, Online::Open, 0), None);
        // Done: nothing, and nothing to draw again for.
        node.turns[0].end = Some(TurnEnd {
            at: 20_000,
            outcome: Outcome::Done,
        });
        node.state = ChatState::Ready;
        assert_eq!(said(&node, 20_000), None);
        assert_eq!(next_second(&node, Online::Open, 20_000), None);
    }

    #[test]
    fn dots_drawn_again_carry_on_where_they_were() {
        let delays = |now| -> Vec<String> {
            (dots(now).children.iter())
                .filter_map(|child| match child {
                    Node::Element(dot) => Some(dot.styles[0].1.clone()),
                    Node::Text(_) => None,
                })
                .collect()
        };
        // Each a little behind the one before.
        assert_eq!(delays(1_000_300), ["-700ms", "-500ms", "-300ms"]);
        // Made a whole number of pulses later, the same.
        assert_eq!(delays(1_000_300 + 3 * 1200), delays(1_000_300));
    }

    #[test]
    fn a_chat_at_work_has_a_running_marker_in_the_list() {
        let mut working = summary(1, None, false, 0);
        working.state = ChatState::Answering;
        let asking = ChatSummary {
            asking: true,
            ..working.clone()
        };
        let ready = summary(3, None, false, 0);
        let dots = |chat: &ChatSummary| {
            (chat_row(chat, 0).find_all("class").into_iter())
                .any(|element| element.get("class") == Some("session-status running"))
        };
        assert!(dots(&working));
        assert!(!dots(&asking), "it waits for you, and says so");
        assert!(!dots(&ready));
    }

    #[test]
    fn an_iphone_in_safari_is_told_to_add_the_app_first() {
        let ways = pairing::Ways {
            password: true,
            ask: true,
        };
        let first = pairing::start(ways, "iPhone", true);
        assert!(first.all_text().contains("Add to Home Screen"));
        assert!(first.find_all("id").is_empty(), "no pairing in Safari");
        let installed = pairing::start(ways, "iPhone", false);
        let ids: Vec<&str> = (installed.find_all("id").into_iter())
            .filter_map(|input| input.get("id"))
            .collect();
        assert_eq!(ids, ["device-name", "password"]);
        assert!(
            pairing::asking("0042")
                .all_text()
                .contains(":Devices allow 0042")
        );
    }
    #[test]
    fn queued_controls_use_stable_ids_and_label_cancelling_fallbacks() {
        let mut node = ChatNode {
            state: ChatState::Answering,
            ..ChatNode::default()
        };
        node.delivery.native_steering = true;
        node.delivery
            .queued
            .push(clackatoo_protocol::QueuedMessage {
                id: 87,
                text: "guide".into(),
                images: 0,
                target: None,
            });
        let shown = queue(&node, Online::Open);
        assert!(shown.all_text().contains("Steer now"));
        assert!(
            shown
                .find_all("data-action")
                .iter()
                .any(|e| e.get("data-action") == Some("remove-queued:87"))
        );
        node.delivery.queued[0].images = 1;
        assert!(
            queue(&node, Online::Open)
                .all_text()
                .contains("Stop & continue")
        );
        assert_eq!(queue(&node, Online::Lost(1)).find_all("disabled").len(), 2);
    }

    #[test]
    fn dashboard_search_and_attention_include_failed_work() {
        let mut state = State::default();
        let mut failed = summary(2, None, false, 0);
        failed.state = ChatState::Failed("offline".into());
        state.chats = vec![summary(1, None, false, 0), failed];
        state.home_filter = crate::state::HomeFilter::Attention;
        let page = home(&state, Notifications::On, 0);
        assert!(page.all_text().contains("chat 2"));
        assert!(!page.all_text().contains("chat 1"));
        state.home_query = "CLAUDE".into();
        assert!(
            home(&state, Notifications::On, 0)
                .all_text()
                .contains("chat 2")
        );
        state.home_query = "nothing".into();
        assert!(
            home(&state, Notifications::On, 0)
                .all_text()
                .contains("No matches")
        );
    }
}
