//! Agent-first remote overview. Filters are local view state; the daemon
//! remains the authority for status, attention and workspace membership.
use std::fmt::Write as _;

use clackatoo_protocol::{Background, BackgroundState, ChatState, TerminalStatus, WorkspaceId};

use crate::action::Action;
use crate::state::{HomeFilter, State};
use crate::ui::{Element, el};
use crate::view::{self, Notifications};

pub(crate) fn home(state: &State, notifications: Notifications, now: u64) -> Element {
    el("main")
        .class("home")
        .child(
            el("header")
                .class("bar home-bar")
                .child(
                    el("img")
                        .class("brand")
                        .attr("src", "/icon-192.png")
                        .attr("alt", ""),
                )
                .child(el("h1").text("Clackatoo"))
                .child(view::online(state.online)),
        )
        .child(
            el("div")
                .class("dashboard")
                .child(
                    el("label")
                        .class("search-box")
                        .child(
                            el("span")
                                .class("sr-only")
                                .text("Search agents and workspaces"),
                        )
                        .child(
                            el("input")
                                .attr("id", "home-search")
                                .attr("type", "search")
                                .attr("placeholder", "Search agents, chats, workspaces")
                                .attr("autocomplete", "off")
                                .attr("value", state.home_query.clone()),
                        ),
                )
                .child(el("div").attr("id", "home-results").child(results(
                    state,
                    notifications,
                    now,
                ))),
        )
}

pub(crate) fn results(state: &State, notifications: Notifications, now: u64) -> Element {
    let mut result = filters(state);
    if state.open.is_none()
        && let Some(note) = &state.note
    {
        result = result.child(
            el("p")
                .class("note")
                .attr("role", "status")
                .text(note.clone()),
        );
    }
    let query = state.home_query.trim().to_lowercase();
    let matches = |text: &str| query.is_empty() || text.to_lowercase().contains(&query);
    let mut workspaces: Vec<(Option<WorkspaceId>, String)> = state
        .workspaces
        .list
        .iter()
        .map(|w| (Some(w.id), w.name.clone()))
        .collect();
    workspaces.push((None, "Elsewhere".into()));
    // A workspace with an approval or failure belongs above quiet workspaces.
    workspaces.sort_by_key(|(id, _)| {
        !state
            .chats
            .iter()
            .any(|c| c.workspace == *id && HomeFilter::Attention.matches(c))
    });
    let mut any = false;
    let mut sections = el("div").class("workspace-grid");
    for (id, name) in workspaces {
        let mut rows = Vec::new();
        if state.home_filter == HomeFilter::Terminals {
            rows.extend(
                state
                    .terminals_of(id)
                    .into_iter()
                    .filter(|t| {
                        matches(&format!(
                            "{name} {} {}",
                            t.title,
                            t.cwd.as_deref().unwrap_or_default()
                        ))
                    })
                    .map(view::terminal_row),
            );
            // Its background tabs, after its terminals.
            let background = id.and_then(|id| state.workspaces.get(id));
            rows.extend(
                (background.into_iter())
                    .flat_map(|workspace| &workspace.background)
                    .filter(|background| matches(&format!("{name} {}", background.name)))
                    .map(|background| background_row(state, background)),
            );
        } else {
            let mut chats: Vec<_> = state
                .chats
                .iter()
                .filter(|c| {
                    c.workspace == id
                        && state.home_filter.matches(c)
                        && matches(&format!("{name} {} {}", c.title, c.agent))
                })
                .collect();
            chats.sort_by_key(|c| {
                (
                    !HomeFilter::Attention.matches(c),
                    !matches!(c.state, ChatState::Answering | ChatState::Starting),
                    c.parent.is_some(),
                    u64::MAX - c.active,
                )
            });
            rows.extend(chats.into_iter().map(|c| {
                view::chat_row(c, now).class_if(
                    state.open.as_ref().is_some_and(|o| o.id == c.chat),
                    "current",
                )
            }));
        }
        if rows.is_empty() {
            continue;
        }
        any = true;
        let mut heading = el("h2")
            .text(name)
            .child(el("span").class("count").text(rows.len().to_string()));
        if let Some(pill) = id.and_then(|id| background_pill(state, id)) {
            heading = heading.child(pill);
        }
        sections = sections.child(
            el("section")
                .class("workspace")
                .child(heading)
                .child(el("ul").class("chats").children(rows)),
        );
    }
    result = result.child(sections);
    if !any {
        result = result.child(empty(state.home_filter, !query.is_empty()));
    }
    notification_banner(result, notifications)
}

fn filters(state: &State) -> Element {
    let attention = state
        .chats
        .iter()
        .filter(|c| HomeFilter::Attention.matches(c))
        .count();
    let working = state
        .chats
        .iter()
        .filter(|c| HomeFilter::Working.matches(c))
        .count();
    // Background tabs count once each, rather than by their terminals.
    let background = state.background_terminals();
    let terminals = state
        .terminals
        .iter()
        .filter(|t| !state.chats.iter().any(|c| c.chat == t.id) && !background.contains(&t.id))
        .count()
        + (state.workspaces.list.iter())
            .map(|workspace| workspace.background.len())
            .sum::<usize>();
    let counts = [state.chats.len(), attention, working, terminals];
    el("div").child(
        el("nav")
            .class("filters")
            .attr("aria-label", "Show sessions")
            .children(
                HomeFilter::ALL
                    .into_iter()
                    .zip(counts)
                    .map(|(filter, count)| {
                        el("button")
                            .class("filter")
                            .class_if(state.home_filter == filter, "selected")
                            .attr("aria-pressed", (state.home_filter == filter).to_string())
                            .action(&Action::Filter(filter))
                            .text(filter.label())
                            .child(el("span").class("count").text(count.to_string()))
                    }),
            ),
    )
}

fn notification_banner(result: Element, notifications: Notifications) -> Element {
    match notifications {
        Notifications::Off => result.child(el("aside").class("banner")
            .child(el("div").child(el("strong").text("Notifications"))
                .child(el("p").text("Approvals and finished work, even when you’re away.")))
            .child(el("button").class("quiet").action(&Action::Notify).text("Enable"))),
        Notifications::Blocked => result.child(el("p").class("notification-note").text("Notifications are blocked in this browser's settings.")),
        Notifications::Unsupported { ios: true } => result.child(el("p").class("notification-note").text("For notifications on iPhone: Share → Add to Home Screen, then open and pair the app there.")),
        _ => result,
    }
}

/// What a workspace's heading says of its background tabs, as a window's
/// icon after its tabs does: how many are running, and how one failed
/// that's not been seen, as `⚙ 2` or `⚙ 2 ✗ 1`. Tapping it lists them, with
/// the terminals. Nothing while none runs or failed.
fn background_pill(state: &State, workspace: WorkspaceId) -> Option<Element> {
    let workspace = state.workspaces.get(workspace)?;
    let running = workspace.background_running();
    let failed = workspace.background_failed();
    let text = match failed {
        Some(code) if running > 0 => format!("⚙ {running} ✗ {code}"),
        Some(code) => format!("⚙ ✗ {code}"),
        None if running > 0 => format!("⚙ {running}"),
        None => return None,
    };
    let mut said = format!("{running} running in the background");
    if let Some(code) = failed {
        let _ = write!(said, ", one failed with exit {code}");
    }
    Some(
        el("button")
            .class("background-pill")
            .class_if(failed.is_some(), "failed")
            .action(&Action::Filter(HomeFilter::Terminals))
            .attr("aria-label", said)
            .text(text),
    )
}

/// A background tab, as a row of the list of terminals: how it's getting
/// on, what it's called, and the ports it listens on, opening its terminal
/// as it's tapped; and beside it, bringing it back and dismissing it.
pub(crate) fn background_row(state: &State, background: &Background) -> Element {
    let tab = background.tab.id;
    let name = match background.name.trim() {
        "" => format!("Background tab {tab}"),
        name => name.to_owned(),
    };
    let (status, mut label) = match (background.state, state.background_failed(background)) {
        (BackgroundState::Ended(_), Some(code)) => ("error", format!("Failed · exit {code}")),
        (BackgroundState::Running, Some(code)) => {
            ("error", format!("Running · a command failed, exit {code}"))
        }
        (BackgroundState::Idle, Some(code)) => ("error", format!("Idle · failed, exit {code}")),
        (BackgroundState::Running, None) => ("running", "Running".to_owned()),
        (BackgroundState::Idle, None) => ("idle", "Idle".to_owned()),
        (BackgroundState::Ended(TerminalStatus::Exited(0)), None) => ("ended", "Ended".to_owned()),
        (BackgroundState::Ended(status), None) => ("ended", format!("Ended · {status}")),
    };
    let ports: Vec<String> = (state.ports_of(tab).iter())
        .map(|port| format!(":{}", port.port))
        .collect();
    if !ports.is_empty() {
        label = format!("{label} · {}", ports.join(" "));
    }
    let kept = (background.tab.layout.panes().iter()).any(|pane| {
        state
            .workspaces
            .kept
            .iter()
            .any(|kept| kept.pane == pane.id)
    });
    if kept {
        label.push_str(" · ↻");
    }
    let mut open = el("button")
        .class("background-open")
        .attr("aria-label", format!("Open {name}"));
    if let Some(terminal) = state.background_terminal(tab) {
        open = open.action(&Action::OpenTerminal(terminal));
    }
    let confirming = state.confirming(tab);
    el("li")
        .class("chat background")
        .class(status)
        .child(
            open.child(
                el("span")
                    .class("session-status")
                    .class(status)
                    .attr("aria-hidden", "true"),
            )
            .child(
                el("span")
                    .class("session-body")
                    .child(el("span").class("title").text(name.clone()))
                    .child(
                        el("span")
                            .class("meta")
                            .child(el("span").class("session-provider").text("Background"))
                            .child(el("span").class("session-state").text(label)),
                    ),
            ),
        )
        .child(
            el("span")
                .class("background-actions")
                .child(
                    el("button")
                        .class("quiet bring")
                        .action(&Action::Bring(tab))
                        .attr("aria-label", format!("Bring {name} back as a tab"))
                        .text("Bring back"),
                )
                .child(
                    el("button")
                        .class("quiet dismiss")
                        .class_if(confirming, "confirm")
                        .action(&Action::Dismiss(tab))
                        .attr(
                            "aria-label",
                            if confirming {
                                format!("Tap again to stop and dismiss {name}")
                            } else {
                                format!("Dismiss {name}")
                            },
                        )
                        .text(if confirming { "Stop?" } else { "✕" }),
                ),
        )
}

fn empty(filter: HomeFilter, searching: bool) -> Element {
    let (title, hint) = if searching {
        ("No matches", "Try another name or switch the filter above.")
    } else if filter == HomeFilter::Attention {
        (
            "You're all caught up",
            "Approvals, new replies and errors will appear here.",
        )
    } else if filter == HomeFilter::Working {
        (
            "Nothing running right now",
            "Your agents will appear here when they start working.",
        )
    } else {
        (
            "Nothing here yet",
            "Start a chat or terminal in a Clackatoo window to see it here.",
        )
    };

    el("div")
        .class("empty")
        .child(el("h3").text(title))
        .child(el("p").text(hint))
}
