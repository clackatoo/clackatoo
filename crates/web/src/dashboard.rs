//! Agent-first remote overview. Filters are local view state; the daemon
//! remains the authority for status, attention and workspace membership.
use clackatoo_protocol::{ChatState, WorkspaceId};

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
        sections = sections.child(
            el("section")
                .class("workspace")
                .child(
                    el("h2")
                        .text(name)
                        .child(el("span").class("count").text(rows.len().to_string())),
                )
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
    let terminals = state
        .terminals
        .iter()
        .filter(|t| !state.chats.iter().any(|c| c.chat == t.id))
        .count();
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
