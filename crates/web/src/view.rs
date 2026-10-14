//! What each screen shows, as [`ui`](crate::ui) trees, from the app's
//! state: the chats and terminals, by workspace, with what needs the user
//! first among them; a chat, as the window shows one: each message, the
//! agent's work folded under one line once it's done, its answer, the
//! cards of the chats it started, and its question, with every choice;
//! and a terminal's header and the row of keys under it (its rows are
//! [`grid`](crate::grid)'s).

use clackatoo_protocol::{
    ChatNode, ChatState, ChatSummary, ChoiceKind, Diff, Item, Outcome, ProviderModels, Setting,
    SettingKind, Subagent, SubagentStatus, TerminalInfo, TerminalStatus, Tool, ToolContent,
    ToolStatus, Turn, TurnFilesState, WorkspaceId,
};

use crate::action::{Action, Pick, RowKey};
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
    let mut page = el("main").class("home").child(
        el("header")
            .class("bar")
            .child(el("h1").text("clackatoo"))
            .child(online(state.online)),
    );
    if let Some(note) = &state.note {
        page = page.child(el("p").class("note").text(note.clone()));
    }
    page = match notifications {
        Notifications::On | Notifications::Unsupported { ios: false } => page,
        Notifications::Off => page.child(
            el("div")
                .class("banner")
                .child(el("span").text("Be told when a chat finishes, or asks you something."))
                .child(el("button").action(&Action::Notify).text("Turn on")),
        ),
        Notifications::Blocked => page.child(
            el("p")
                .class("banner")
                .text("Notifications are blocked: allow them in the browser's settings."),
        ),
        Notifications::Unsupported { ios: true } => page.child(el("p").class("banner").text(
            "On iPhone, notifications come once clackatoo is on the home screen: Share, \
             then Add to Home Screen, then open it from there.",
        )),
    };
    let mut workspaces: Vec<(Option<WorkspaceId>, String)> = (state.workspaces.list.iter())
        .map(|workspace| (Some(workspace.id), workspace.name.clone()))
        .collect();
    workspaces.push((None, "Elsewhere".to_owned()));
    let mut any = false;
    for (id, name) in workspaces {
        let mut chats: Vec<&ChatSummary> = (state.chats.iter())
            .filter(|chat| chat.workspace == id)
            .collect();
        let terminals = state.terminals_of(id);
        if chats.is_empty() && terminals.is_empty() {
            continue;
        }
        any = true;
        // What needs the user first, then the latest.
        chats.sort_by_key(|chat| {
            (
                !chat.needs_you(),
                chat.parent.is_some(),
                u64::MAX - chat.active,
            )
        });
        let mut section = el("section").class("workspace").child(el("h2").text(name));
        if !chats.is_empty() {
            let rows = chats.into_iter().map(|chat| chat_row(chat, now));
            section = section.child(el("ul").class("chats").children(rows));
        }
        if !terminals.is_empty() {
            let rows = terminals.into_iter().map(terminal_row);
            section = section.child(el("ul").class("chats terminals").children(rows));
        }
        page = page.child(section);
    }
    if !any && state.online == Online::Open {
        page = page.child(
            el("p")
                .class("empty")
                .text("No chats or terminals yet: start a chat in a window with :Agent."),
        );
    }
    page
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
fn terminal_row(terminal: &TerminalInfo) -> Element {
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
        .child(el("button").class("back").action(&Action::Home).text("‹"))
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
fn online(online: Online) -> Element {
    let (class, said) = match online {
        Online::Open => ("up", String::new()),
        Online::Connecting => ("connecting", "Connecting…".to_owned()),
        Online::Lost(retry) => ("lost", format!("Offline: trying again in {retry}s")),
    };
    el("span").class("online").class(class).text(said)
}

/// A chat, as a row of the list.
fn chat_row(chat: &ChatSummary, now: u64) -> Element {
    let mut row = el("li")
        .class("chat")
        .class_if(chat.parent.is_some(), "sub")
        .class_if(chat.needs_you(), "needs")
        .action(&Action::Open(chat.chat));
    let mut top = el("div")
        .class("top")
        .child(el("span").class("title").text(chat.title.clone()));
    if chat.asking {
        top = top.child(el("span").class("badge").text("Asks you"));
    } else if matches!(chat.state, ChatState::Starting | ChatState::Answering) {
        top = top.child(dots(now));
    } else if chat.unseen {
        top = top.child(el("span").class("badge new").text("New"));
    }
    row = row.child(top);
    let mut said = vec![chat.agent.clone(), state_word(&chat.state).to_owned()];
    if chat.active > 0 {
        said.push(ago(now.saturating_sub(chat.active)));
    }
    row.child(el("div").class("meta").text(said.join(" · ")))
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
        if let Some(usage) = node.usage.filter(|usage| usage.size > 0) {
            said.push(format!("{}% of context", usage.used * 100 / usage.size));
        }
    }
    if let Online::Lost(retry) = state.online {
        said.push(format!("offline, trying again in {retry}s"));
    }
    let mut header = el("header")
        .class("bar chat-bar")
        .child(el("button").class("back").action(&Action::Home).text("‹"))
        .child(
            el("div")
                .class("heading")
                .child(el("h1").text(title))
                .child(el("div").class("meta").text(said.join(" · "))),
        );
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
        .attr("data-turn", index.to_string());
    let Some(turn) = node.turns.get(index) else {
        return section;
    };
    if let Some(switch) = &turn.switch {
        return section
            .class("switch")
            .child(el("div").class("divider").text(switch.label()));
    }
    let mut section = section;
    if !turn.prompt.is_empty() {
        let mut prompt = el("div").class("prompt");
        if let Some(from) = &turn.from {
            prompt = prompt.child(el("div").class("from").text(format!("From {}", from.title)));
        }
        section = section.child(prompt.child(el("div").class("bubble").text(turn.prompt.clone())));
    }
    let answer = answer_start(turn);
    let running = turn.end.is_none();
    if answer > 0 || running {
        let key = format!("t{index}");
        let opened = open.folds.get(&key).copied().unwrap_or(running);
        let label = fold_label(turn);
        let mut work = el("details")
            .class("work")
            .attr("data-fold", key)
            .child(el("summary").text(label));
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
/// while it goes, that it's going. What it's doing, and for how long, is
/// said just above the box (see [`working`]), which is always in view and
/// counts the seconds without the turn being drawn again.
fn fold_label(turn: &Turn) -> String {
    let Some(end) = &turn.end else {
        return "Working…".to_owned();
    };
    if turn.sent == 0 {
        return "Worked".to_owned();
    }
    format!("Worked for {}", duration(end.at.saturating_sub(turn.sent)))
}

/// What the line just above the box says while the agent works, at
/// `now`: what it's doing, "Running cargo test…", and how long the turn
/// going has been going, "12s", or nothing if its seconds don't count,
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
        .map(|sent| duration(now.saturating_sub(sent)))
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
        // agent would do, however it's worded.
        let asked = if question.own {
            question.title.clone()
        } else {
            format!("Allow the agent to {}?", question.title)
        };
        footer = footer.child(
            el("div")
                .class("question")
                .child(el("p").text(asked))
                .child(el("div").class("choices").children(choices)),
        );
    }
    footer
}

/// The shown chat's menus: its model, among its agent's and the other
/// agents', its mode, its effort, and what it may do with clackatoo's
/// tools.
#[must_use]
pub fn menus(node: &ChatNode) -> Element {
    let mut menus = el("div").class("menus");
    let order = [
        SettingKind::Model,
        SettingKind::Mode,
        SettingKind::Effort,
        SettingKind::Tools,
    ];
    for kind in order {
        for setting in node.settings.iter().filter(|setting| setting.kind == kind) {
            let mut select = setting_menu(setting);
            if kind == SettingKind::Model {
                select = select.children(node.providers.iter().map(|provider| {
                    let options: Vec<Element> = match &provider.models {
                        ProviderModels::Known(models) if !models.is_empty() => (models.iter())
                            .map(|model| {
                                el("option")
                                    .attr(
                                        "value",
                                        Pick::switch_value(&provider.agent, Some(&model.value)),
                                    )
                                    .text(format!("{} · {}", provider.title, model.name))
                            })
                            .collect(),
                        models => {
                            let said = match models {
                                ProviderModels::Learning => " (finding out…)",
                                ProviderModels::NotInstalled => " (to install)",
                                ProviderModels::Failed(_) => " (couldn't say)",
                                _ => "",
                            };
                            vec![
                                el("option")
                                    .attr("value", Pick::switch_value(&provider.agent, None))
                                    .text(format!("{}{said}", provider.title)),
                            ]
                        }
                    };
                    el("optgroup")
                        .attr("label", provider.title.clone())
                        .children(options)
                }));
            }
            menus = menus.child(
                el("label")
                    .class("menu")
                    .child(el("span").text(setting.name.clone()))
                    .child(select),
            );
        }
    }
    menus
}

/// A menu of `setting`'s choices, with its value picked.
fn setting_menu(setting: &Setting) -> Element {
    let options = setting.choices.iter().map(|choice| {
        let option = el("option")
            .attr("value", Pick::set_value(&choice.value))
            .text(choice.name.clone());
        if choice.value == setting.value {
            option.attr("selected", "")
        } else {
            option
        }
    });
    el("select")
        .attr("data-setting", setting.id.clone())
        .children(options)
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
        Choice, Location, Question, SettingChoice, TerminalId, TurnEnd, Workspace, Workspaces,
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
            list: vec![Workspace {
                id: WorkspaceId(1),
                name: "clackatoo".into(),
                tabs: Vec::new(),
                active_tab: clackatoo_protocol::TabId(1),
            }],
            active: None,
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
        assert_eq!(opened, ["notify", "open:2", "open:1", "open:3"]);
        let text = page.all_text();
        assert!(text.contains("clackatoo"));
        assert!(text.contains("Asks you"));
        assert!(text.contains("Elsewhere"));
        assert!(text.contains("Claude Code · ready · 5m"), "{text}");
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

    #[test]
    fn the_model_menu_offers_other_agents() {
        let mut node = ChatNode::default();
        node.settings.push(Setting {
            id: "model".into(),
            name: "Model".into(),
            kind: SettingKind::Model,
            value: "sonnet".into(),
            choices: ["haiku", "sonnet"]
                .into_iter()
                .map(|value| SettingChoice {
                    value: value.into(),
                    name: value.into(),
                    description: None,
                })
                .collect(),
        });
        node.providers.push(clackatoo_protocol::Provider {
            agent: "codex".into(),
            title: "Codex".into(),
            models: ProviderModels::Unknown,
        });
        let menus = menus(&node);
        let select = &menus.find_all("data-setting")[0];
        let values: Vec<String> = (select.children.iter())
            .flat_map(|child| match child {
                Node::Element(option) if option.tag == "option" => vec![option.clone()],
                Node::Element(group) => (group.children.iter())
                    .filter_map(|child| match child {
                        Node::Element(option) => Some(option.clone()),
                        Node::Text(_) => None,
                    })
                    .collect(),
                Node::Text(_) => Vec::new(),
            })
            .map(|option| option.get("value").unwrap().to_owned())
            .collect();
        assert_eq!(values, ["set:haiku", "set:sonnet", "switch:codex:"]);
    }

    #[test]
    fn terminals_are_listed_beside_the_chats_saying_what_and_where() {
        let mut state = State::default();
        state.workspaces = Workspaces {
            list: vec![Workspace {
                id: WorkspaceId(1),
                name: "clackatoo".into(),
                tabs: vec![clackatoo_protocol::Tab {
                    id: clackatoo_protocol::TabId(1),
                    layout: clackatoo_protocol::Layout::Pane(clackatoo_protocol::Pane {
                        id: clackatoo_protocol::PaneId(1),
                        content: clackatoo_protocol::Content::Terminal(TerminalId(2)),
                    }),
                    focused: clackatoo_protocol::PaneId(1),
                    name: None,
                }],
                active_tab: clackatoo_protocol::TabId(1),
            }],
            active: None,
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
        let page = home(&state, Notifications::On, 0);
        let opened: Vec<&str> = (page.find_all("data-action").into_iter())
            .filter_map(|row| row.get("data-action"))
            .collect();
        assert_eq!(opened, ["term:2"]);
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
            Some("Running cargo test… 12s")
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
            work.all_text().starts_with("Working…"),
            "{}",
            work.all_text()
        );
        assert_eq!(turn(&node, &open(), 0, 99_000), section);
        // Thinking, then writing.
        node.turns[0].items = vec![Item::Thought("hmm".into())];
        assert_eq!(said(&node, 13_400).as_deref(), Some("Thinking… 12s"));
        node.turns[0].items.push(Item::Words("It's".into()));
        assert_eq!(said(&node, 13_400).as_deref(), Some("Writing… 12s"));
        // Asking you, it waits: nothing.
        node.question = Some(Question {
            id: 1,
            title: "run ls".into(),
            choices: Vec::new(),
            own: false,
        });
        assert_eq!(said(&node, 13_400), None);
        node.question = None;
        // Starting, before any message: no seconds.
        let starting = ChatNode {
            agent: "Claude Code".into(),
            ..ChatNode::default()
        };
        assert_eq!(
            working(&starting, Online::Open, 0),
            Some(("Starting Claude Code…".to_owned(), String::new()))
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
    fn a_chat_at_work_has_dots_in_the_list() {
        let mut working = summary(1, None, false, 0);
        working.state = ChatState::Answering;
        let asking = ChatSummary {
            asking: true,
            ..working.clone()
        };
        let ready = summary(3, None, false, 0);
        let dots = |chat: &ChatSummary| {
            (chat_row(chat, 0).find_all("class").into_iter())
                .any(|element| element.get("class") == Some("dots"))
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
}
