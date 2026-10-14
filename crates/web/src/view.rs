//! What each screen shows, as [`ui`](crate::ui) trees, from the app's
//! state: the chats, by workspace, with what needs the user first among
//! them; and a chat, as the window shows one: each message, the agent's
//! work folded under one line once it's done, its answer, the cards of
//! the chats it started, and its question, with every choice.

use clackatoo_protocol::{
    ChatNode, ChatState, ChatSummary, ChoiceKind, Diff, Item, Outcome, ProviderModels, Setting,
    SettingKind, Subagent, SubagentStatus, Tool, ToolContent, ToolStatus, Turn, TurnFilesState,
    WorkspaceId,
};

use crate::action::{Action, Pick};
use crate::markdown;
use crate::state::{Online, OpenChat, State};
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
        if chats.is_empty() {
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
        let rows = chats.into_iter().map(|chat| chat_row(chat, now));
        page = page.child(
            el("section")
                .class("workspace")
                .child(el("h2").text(name))
                .child(el("ul").class("chats").children(rows)),
        );
    }
    if !any && state.online == Online::Open {
        page = page.child(
            el("p")
                .class("empty")
                .text("No chats yet: start one in a window with :Agent."),
        );
    }
    page
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
        let label = fold_label(node, turn, now);
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
/// while it goes, how long it's been going, and what it's doing.
fn fold_label(node: &ChatNode, turn: &Turn, now: u64) -> String {
    let Some(end) = &turn.end else {
        let elapsed = if turn.sent == 0 {
            String::new()
        } else {
            format!(" {}", duration(now.saturating_sub(turn.sent)))
        };
        let doing = match node.state {
            ChatState::Starting => format!("Starting {}…{elapsed}", node.agent),
            _ => format!("Working…{elapsed}"),
        };
        let now_doing = (turn.items.iter().rev()).find_map(|item| match item {
            Item::Tool(tool) if tool.status == ToolStatus::Running => Some(tool.title.as_str()),
            _ => None,
        });
        return match now_doing {
            Some(what) => format!("{doing} · {what}"),
            None => doing,
        };
    };
    if turn.sent == 0 {
        return "Worked".to_owned();
    }
    format!("Worked for {}", duration(end.at.saturating_sub(turn.sent)))
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
