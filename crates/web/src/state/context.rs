//! Phone context controls use the same daemon selectors and snapshots as the window.
use super::{Asked, State};
use crate::action::Action;
use crate::ui::{Element, el};
use clackatoo_protocol::{
    ContextAction, ContextCandidate, ContextKind, ContextMode, ContextRef, ContextReply,
    ContextSelection, ContextSnapshot, Request, TerminalId,
};
use std::collections::BTreeMap;

/// A context control, encoded in the delegated DOM action listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Open,
    Kind(usize),
    Pick(usize),
    Attach,
    Remove(usize),
    Inspect(usize),
    Mode,
    Refresh,
    Narrow,
    Read(u32, usize),
    Next,
    Previous,
}
impl Control {
    pub(crate) fn encode(&self) -> String {
        match self {
            Self::Open => "open".into(),
            Self::Kind(i) => format!("kind:{i}"),
            Self::Pick(i) => format!("pick:{i}"),
            Self::Attach => "attach".into(),
            Self::Remove(i) => format!("remove:{i}"),
            Self::Inspect(i) => format!("inspect:{i}"),
            Self::Mode => "mode".into(),
            Self::Refresh => "refresh".into(),
            Self::Narrow => "narrow".into(),
            Self::Read(t, i) => format!("read:{t}:{i}"),
            Self::Next => "next".into(),
            Self::Previous => "previous".into(),
        }
    }
    pub(crate) fn decode(s: &str) -> Option<Self> {
        let (word, n) = s.split_once(':').unwrap_or((s, ""));
        Some(match (word, n) {
            ("open", "") => Self::Open,
            ("kind", n) => Self::Kind(n.parse().ok()?),
            ("pick", n) => Self::Pick(n.parse().ok()?),
            ("attach", "") => Self::Attach,
            ("remove", n) => Self::Remove(n.parse().ok()?),
            ("inspect", n) => Self::Inspect(n.parse().ok()?),
            ("mode", "") => Self::Mode,
            ("refresh", "") => Self::Refresh,
            ("narrow", "") => Self::Narrow,
            ("read", n) => {
                let (t, i) = n.split_once(':')?;
                Self::Read(t.parse().ok()?, i.parse().ok()?)
            }
            ("next", "") => Self::Next,
            ("previous", "") => Self::Previous,
            _ => return None,
        })
    }
}
const KINDS: [(ContextKind, &str); 9] = [
    (ContextKind::All, "All"),
    (ContextKind::File, "Files"),
    (ContextKind::Symbol, "Symbols"),
    (ContextKind::Diagnostics, "Diagnostics"),
    (ContextKind::Command, "Commands"),
    (ContextKind::Change, "Changes"),
    (ContextKind::Chat, "Chats"),
    (ContextKind::Board, "Boards"),
    (ContextKind::Project, "Folders"),
];
#[derive(Debug, Default)]
pub(crate) struct Context {
    pub attached: BTreeMap<TerminalId, Vec<ContextSelection>>,
    pub synced: BTreeMap<TerminalId, Vec<ContextSelection>>,
    pub(super) touched: std::collections::BTreeSet<TerminalId>,
    open: bool,
    pub(super) chat: Option<TerminalId>,
    kind: usize,
    pub query: String,
    pub range: String,
    items: Vec<ContextCandidate>,
    preview: Option<ContextSnapshot>,
    selected: usize,
    historical: bool,
    editing: Option<usize>,
    offset: u32,
    more: bool,
    mode: ContextMode,
    pub(super) serial: u64,
    pub(super) status: String,
}
impl State {
    fn context_request(&mut self, action: ContextAction) {
        let Some(chat) = self.open.as_ref().map(|o| o.id) else {
            return;
        };
        self.context.serial = self.context.serial.wrapping_add(1);
        self.context.chat = Some(chat);
        let serial = self.context.serial;
        self.request(
            Request::Context { chat, action },
            Asked::Context { chat, serial },
        );
        self.dirty.footer = true;
    }
    pub(crate) fn context_search(&mut self, query: &str) {
        self.context.query = query.chars().take(1024).collect();
        self.context.preview = None;
        self.context.editing = None;
        self.context.historical = false;
        self.context.status = "Searching…".into();
        self.context_request(ContextAction::Search {
            kind: KINDS[self.context.kind].0,
            query: self.context.query.clone(),
            offset: self.context.offset,
        });
    }
    #[allow(clippy::too_many_lines, reason = "one action per context control")]
    pub(crate) fn context_control(&mut self, control: Control) {
        let Some(chat) = self.open.as_ref().map(|o| o.id) else {
            return;
        };
        match control {
            Control::Open => {
                self.context.open = !self.context.open;
                if self.context.open {
                    self.context.offset = 0;
                    self.context_search(&self.context.query.clone());
                }
            }
            Control::Kind(i) if i < KINDS.len() => {
                self.context.kind = i;
                self.context.offset = 0;
                self.context_search(&self.context.query.clone());
            }
            Control::Inspect(i) => {
                if let Some(selection) = self
                    .context
                    .attached
                    .get(&chat)
                    .and_then(|a| a.get(i))
                    .cloned()
                {
                    self.context.items = vec![ContextCandidate {
                        kind: ContextKind::All,
                        label: source_label(&selection.source),
                        detail: "Attached context".into(),
                        source: selection.source.clone(),
                    }];
                    self.context.selected = 0;
                    self.context.editing = Some(i);
                    self.context.preview = None;
                    self.context.historical = false;
                    self.context_request(ContextAction::Preview(ContextSelection {
                        expected: None,
                        ..selection
                    }));
                }
            }
            Control::Pick(i) => {
                self.context.editing = None;
                if let Some(item) = self.context.items.get(i) {
                    self.context.selected = i;
                    self.context.preview = None;
                    self.context.historical = false;
                    self.context.status = "Loading preview…".into();
                    self.context_request(ContextAction::Preview(ContextSelection {
                        source: item.source.clone(),
                        mode: self.context.mode,
                        expected: None,
                    }));
                }
            }
            Control::Attach => {
                if let Some(snapshot) = &self.context.preview {
                    let snapshot = snapshot.clone();
                    let source = if self.context.historical {
                        ContextRef::Retained {
                            id: snapshot.id.clone(),
                            grant: snapshot.grant.clone(),
                        }
                    } else {
                        self.context
                            .items
                            .get(self.context.selected)
                            .map_or_else(|| snapshot.source.clone(), |item| item.source.clone())
                    };
                    let expected = match &source {
                        ContextRef::Owned { source, .. }
                            if matches!(
                                source.as_ref(),
                                ContextRef::File { lines: None, .. }
                                    | ContextRef::Diagnostics { .. }
                                    | ContextRef::Command { .. }
                                    | ContextRef::Project { .. }
                            ) =>
                        {
                            None
                        }
                        ContextRef::File { lines: None, .. }
                        | ContextRef::Diagnostics { .. }
                        | ContextRef::Command { .. }
                        | ContextRef::Project { .. } => None,
                        _ => Some(snapshot.revision.clone()),
                    };
                    let selection = ContextSelection {
                        source,
                        mode: snapshot.mode,
                        expected,
                    };
                    let count = self.context.attached.get(&chat).map_or(0, Vec::len)
                        + self.images.get(&chat).map_or(0, Vec::len);
                    let attached = self.context.attached.entry(chat).or_default();
                    if let Some(old) = self.context.editing.and_then(|i| attached.get_mut(i)) {
                        *old = selection;
                    } else if let Some(old) =
                        attached.iter_mut().find(|s| s.source == selection.source)
                    {
                        *old = selection;
                    } else if count < ContextSnapshot::MAX_COUNT {
                        attached.push(selection);
                    } else {
                        self.context.status = "At most 32 attachments per message.".into();
                        return;
                    }
                    self.context.touched.insert(chat);
                    self.context.status = "Attached. Close to write your message.".into();
                    self.tell_draft();
                }
            }
            Control::Remove(i) => {
                if let Some(a) = self.context.attached.get_mut(&chat)
                    && i < a.len()
                {
                    a.remove(i);
                    self.context.editing = None;
                    self.context.touched.insert(chat);
                    self.tell_draft();
                }
            }
            Control::Mode => {
                self.context.mode = if self.context.mode == ContextMode::Contents {
                    ContextMode::Reference
                } else {
                    ContextMode::Contents
                };
                self.context_control(Control::Pick(self.context.selected));
            }
            Control::Narrow => self.context_narrow(),
            Control::Refresh => {
                if self.context.preview.is_some() {
                    self.context_control(Control::Pick(self.context.selected));
                } else {
                    self.context_search(&self.context.query.clone());
                }
            }
            Control::Read(turn, index) => {
                let snapshot = self
                    .open
                    .as_ref()
                    .and_then(|o| o.view.as_ref()?.chat())
                    .and_then(|n| n.turns.get(turn.checked_sub(n.first_turn)? as usize))
                    .and_then(|t| t.context.get(index))
                    .cloned();
                if let Some(snapshot) = snapshot {
                    self.context.open = true;
                    self.context.historical = true;
                    self.context.preview = None;
                    self.context.status = "Loading captured context…".into();
                    self.context_request(ContextAction::Read {
                        turn,
                        id: snapshot.id,
                    });
                }
            }
            Control::Next if self.context.more => {
                self.context.offset += 100;
                self.context_search(&self.context.query.clone());
            }
            Control::Previous => {
                self.context.offset = self.context.offset.saturating_sub(100);
                self.context_search(&self.context.query.clone());
            }
            _ => {}
        }
        self.dirty.footer = true;
    }
    fn context_narrow(&mut self) {
        let Some((a, b)) = self.context.range.split_once('-').and_then(|(a, b)| {
            Some((a.trim().parse::<u64>().ok()?, b.trim().parse::<u64>().ok()?))
        }) else {
            self.context.status = "Enter an inclusive range such as 12-40.".into();
            return;
        };
        let Some(preview) = &self.context.preview else {
            return;
        };
        let source = match &preview.source {
            ContextRef::File { path, .. } | ContextRef::Symbol { path, .. } => {
                let (Ok(a), Ok(b)) = (u32::try_from(a), u32::try_from(b)) else {
                    self.context.status = "Invalid source line range.".into();
                    return;
                };
                ContextRef::File {
                    path: path.clone(),
                    lines: Some((a, b)),
                }
            }
            ContextRef::Terminal { terminal, .. } | ContextRef::Command { terminal, .. } => {
                ContextRef::Terminal {
                    terminal: *terminal,
                    first: a,
                    last: b,
                }
            }
            _ => {
                self.context.status = "Select a file, symbol or terminal excerpt to narrow.".into();
                return;
            }
        };
        let source = if let Some(ContextRef::Owned { machine, .. }) = self
            .context
            .items
            .get(self.context.selected)
            .map(|i| &i.source)
        {
            ContextRef::Owned {
                machine: *machine,
                source: Box::new(source),
            }
        } else {
            source
        };
        self.context.items = vec![ContextCandidate {
            kind: ContextKind::All,
            label: preview.label.clone(),
            detail: "Selected range".into(),
            source,
        }];
        let editing = self.context.editing;
        self.context_control(Control::Pick(0));
        self.context.editing = editing;
    }
    pub(super) fn context_answer(&mut self, chat: TerminalId, serial: u64, reply: ContextReply) {
        if self.context.chat != Some(chat) || self.context.serial != serial {
            return;
        }
        match reply {
            ContextReply::Candidates {
                items,
                more,
                notice,
            } => {
                self.context.items = items;
                self.context.more = more;
                self.context.status = if notice.is_empty() {
                    "Choose a source to preview. Search is bounded; symbol results reflect index coverage.".into()
                } else {
                    notice
                };
            }
            ContextReply::Snapshot(snapshot) => {
                self.context.preview = Some(snapshot);
                self.context.status = "Inspect this snapshot, then attach it.".into();
            }
            ContextReply::Draft { .. } => {}
        }
        self.dirty.footer = true;
    }
    pub(super) fn context_draft_came(
        &mut self,
        chat: TerminalId,
        selections: Vec<ContextSelection>,
    ) {
        if !self.context.touched.contains(&chat) {
            self.context.attached.insert(chat, selections.clone());
            self.context.synced.insert(chat, selections);
        }
    }
    pub(crate) fn context_panel(&self) -> Element {
        let button = |label: &str, control: Control| {
            el("button")
                .class_if(control != Control::Attach, "quiet")
                .action(&Action::Context(control))
                .text(label)
        };
        let Some(chat) = self.open.as_ref().map(|o| o.id) else {
            return el("div");
        };
        let count = self.context.attached.get(&chat).map_or(0, Vec::len);
        let mut panel = el("div").child(button(&format!("+ Context · {count}"), Control::Open));
        if !self.context.open || self.context.chat != Some(chat) {
            return panel;
        }
        let mut body =
            el("div")
                .class("context-browser")
                .child(el("nav").class("context-categories").children(
                    KINDS.iter().enumerate().map(|(i, (_, label))| {
                        button(label, Control::Kind(i))
                            .attr("aria-pressed", (i == self.context.kind).to_string())
                    }),
                ))
                .child(
                    el("input")
                        .attr("id", "context-search")
                        .attr("type", "search")
                        .attr("placeholder", "Find workspace context…")
                        .attr("value", self.context.query.clone())
                        .attr("aria-label", "Find workspace context"),
                );
        for (i, selection) in self
            .context
            .attached
            .get(&chat)
            .into_iter()
            .flatten()
            .enumerate()
        {
            body = body.child(
                el("div")
                    .class("context-attached")
                    .child(button(
                        &source_label(&selection.source),
                        Control::Inspect(i),
                    ))
                    .child(button("Remove", Control::Remove(i))),
            );
        }
        body = body.child(el("p").class("muted").text(&self.context.status));
        if let Some(snapshot) = &self.context.preview {
            body = body
                .child(el("h3").text(&snapshot.label))
                .child(el("p").text(format!(
                    "{} · {:?} · {} bytes · about {} tokens",
                    snapshot.location,
                    snapshot.mode,
                    snapshot.bytes,
                    snapshot.bytes / 4
                )))
                .child(el("p").class("muted").text(&snapshot.note))
                .child(el("pre").class("context-preview").text(&snapshot.text))
                .child(button("Attach", Control::Attach));
            if !self.context.historical {
                body = body
                    .child(button("Refresh", Control::Refresh))
                    .child(button("Contents / reference", Control::Mode))
                    .child(
                        el("input")
                            .attr("id", "context-range")
                            .attr("placeholder", "Lines, for example 12-40")
                            .attr("aria-label", "Context line range")
                            .attr("value", self.context.range.clone()),
                    )
                    .child(button("Narrow lines", Control::Narrow));
            }
        }
        if !self.context.historical {
            body = body
                .child(el("div").class("context-results").children(
                    self.context.items.iter().enumerate().map(|(i, item)| {
                        button(&item.label, Control::Pick(i)).attr("title", &item.detail)
                    }),
                ))
                .child(button("Previous", Control::Previous));
            if self.context.more {
                body = body.child(button("More", Control::Next));
            }
        }
        panel = panel.child(crate::view::sheet(
            "Workspace context",
            &Action::Context(Control::Open),
            body,
        ));
        panel
    }
}
fn source_label(source: &ContextRef) -> String {
    match source {
        ContextRef::File { path, lines } => {
            format!("{path}{}", clackatoo_protocol::lines_suffix(*lines))
        }
        ContextRef::Symbol { name, path, .. } => format!("{name} · {path}"),
        ContextRef::Retained { id, .. } => {
            format!("Captured context · {}", &id[..id.len().min(12)])
        }
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_round_trip_and_unknown_actions_do_nothing() {
        for c in [
            Control::Open,
            Control::Kind(8),
            Control::Pick(99),
            Control::Attach,
            Control::Remove(31),
            Control::Mode,
            Control::Refresh,
            Control::Read(42, 3),
            Control::Next,
            Control::Previous,
        ] {
            assert_eq!(Control::decode(&c.encode()), Some(c));
        }
        assert!(Control::decode("read:broken").is_none());
    }
    #[test]
    fn context_only_submissions_keep_newer_attachments_until_acknowledged() {
        let mut state = State::default();
        state.connected_now();
        state.take_outbox();
        state.asked.clear();
        state.open = Some(super::super::OpenChat {
            id: TerminalId(4),
            capabilities: vec![],
            picker: crate::settings::Picker::default(),
            view: None,
            folds: BTreeMap::default(),
            unacknowledged: false,
            history_pending: None,
        });
        let selected = ContextSelection {
            source: ContextRef::File {
                path: "src/main.rs".into(),
                lines: Some((2, 5)),
            },
            mode: ContextMode::Contents,
            expected: Some("revision".into()),
        };
        state
            .context
            .attached
            .insert(TerminalId(4), vec![selected.clone()]);
        state.submit(false);
        let sent = state.take_outbox();
        assert!(
            matches!(&sent[0],clackatoo_protocol::ClientMessage::Request(Request::Chat{action:clackatoo_protocol::ChatAction::SubmitContext{text,context,..},..}) if text.is_empty()&&context==&vec![selected.clone()])
        );
        let newer = ContextSelection {
            source: ContextRef::Diagnostics { path: None },
            ..selected.clone()
        };
        state
            .context
            .attached
            .get_mut(&TerminalId(4))
            .unwrap()
            .push(newer.clone());
        state.receive(clackatoo_protocol::ServerMessage::Response(
            clackatoo_protocol::Response::Done,
        ));
        assert_eq!(state.context.attached[&TerminalId(4)], vec![newer]);
    }
}
