//! A shared context picker, preview and draft attachment list.
use super::ChatView;
use clackatoo_protocol::{
    ContextAction, ContextCandidate, ContextKind, ContextMode, ContextReply, ContextSelection,
    ContextSnapshot, Response,
};
use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Debug)]
pub(super) struct Attached {
    pub ready: bool,
    pub selection: ContextSelection,
    pub preview: ContextSnapshot,
}
#[derive(Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent picker, request and delivery states"
)]
pub(super) struct ContextUi {
    pub dirty: bool,
    pub scroll: f32,
    pub max_scroll: f32,
    pub attached_offset: usize,
    pub narrowing: bool,
    pub editing: Option<usize>,
    pub range: String,
    pub open: bool,
    pub kind: ContextKind,
    pub query: String,
    pub items: Vec<ContextCandidate>,
    pub selected: usize,
    pub preview: Option<ContextSnapshot>,
    pub attached: Vec<Attached>,
    pub history: Vec<ContextSnapshot>,
    pub history_turn: Option<u32>,
    pub status: String,
    pub more: bool,
    pub offset: u32,
    pub mode: ContextMode,
    next: u64,
    pending: u64,
    requests: Vec<(u64, ContextAction)>,
    automatic: std::collections::BTreeMap<u64, clackatoo_protocol::ContextRef>,
}
pub(super) const KINDS: [ContextKind; 9] = [
    ContextKind::All,
    ContextKind::File,
    ContextKind::Symbol,
    ContextKind::Diagnostics,
    ContextKind::Command,
    ContextKind::Change,
    ContextKind::Chat,
    ContextKind::Board,
    ContextKind::Project,
];

impl ContextUi {
    pub(super) fn has_requests(&self) -> bool {
        !self.requests.is_empty() || self.dirty
    }
    pub(super) fn selections(&self) -> Vec<ContextSelection> {
        self.attached.iter().map(|a| a.selection.clone()).collect()
    }
    fn request(&mut self, action: ContextAction) {
        self.next += 1;
        self.pending = self.next;
        self.requests
            .retain(|(serial, _)| self.automatic.contains_key(serial));
        self.requests.push((self.next, action));
    }
    pub(super) fn search(&mut self) {
        self.history.clear();
        self.editing = None;
        self.history_turn = None;
        self.preview = None;
        self.status = "Searching…".into();
        self.request(ContextAction::Search {
            kind: self.kind,
            query: self.query.clone(),
            offset: self.offset,
        });
    }
    pub(super) fn select(&mut self, index: usize) {
        if let Some(snapshot) = self.history.get(index) {
            let snapshot = snapshot.clone();
            self.selected = index;
            self.preview = Some(snapshot.clone());
            if snapshot.text.len() != snapshot.bytes as usize
                && let Some(turn) = self.history_turn
            {
                self.status = "Loading the retained snapshot…".into();
                self.request(ContextAction::Read {
                    turn,
                    id: snapshot.id,
                });
            }
            return;
        }
        if index >= self.items.len() {
            return;
        }
        self.selected = index;
        self.scroll = 0.0;
        self.preview = None;
        self.status = "Loading preview…".into();
        self.request(ContextAction::Preview(ContextSelection {
            source: self.items[index].source.clone(),
            mode: self.mode,
            expected: None,
        }));
    }
    pub(super) fn inspect_attached(&mut self, index: usize) {
        if let Some(a) = self.attached.get(index) {
            self.editing = Some(index);
            self.items = vec![ContextCandidate {
                kind: ContextKind::All,
                label: a.preview.label.clone(),
                detail: a.preview.location.clone(),
                source: a.selection.source.clone(),
            }];
            self.history.clear();
            self.history_turn = None;
            self.selected = 0;
            self.scroll = 0.0;
            self.preview = Some(a.preview.clone());
            if a.preview.id.is_empty() {
                self.select(0);
            }
        }
    }
    fn narrow(&mut self) {
        let Some((first, last)) = self.range.split_once('-').and_then(|(a, b)| {
            Some((a.trim().parse::<u64>().ok()?, b.trim().parse::<u64>().ok()?))
        }) else {
            self.status = "Enter an inclusive range, for example 12-40.".into();
            return;
        };
        let Some(preview) = &self.preview else { return };
        let source = match &preview.source {
            clackatoo_protocol::ContextRef::File { path, .. }
            | clackatoo_protocol::ContextRef::Symbol { path, .. } => {
                let (Ok(first), Ok(last)) = (u32::try_from(first), u32::try_from(last)) else {
                    self.status = "Invalid source line range.".into();
                    return;
                };
                clackatoo_protocol::ContextRef::File {
                    path: path.clone(),
                    lines: Some((first, last)),
                }
            }
            clackatoo_protocol::ContextRef::Terminal { terminal, .. }
            | clackatoo_protocol::ContextRef::Command { terminal, .. } => {
                clackatoo_protocol::ContextRef::Terminal {
                    terminal: *terminal,
                    first,
                    last,
                }
            }
            _ => {
                self.status = "Select a file, symbol or terminal to narrow its lines.".into();
                return;
            }
        };
        let source = if let Some(clackatoo_protocol::ContextRef::Owned { machine, .. }) =
            self.items.get(self.selected).map(|i| &i.source)
        {
            clackatoo_protocol::ContextRef::Owned {
                machine: *machine,
                source: Box::new(source),
            }
        } else {
            source
        };
        self.items = vec![ContextCandidate {
            kind: ContextKind::All,
            label: preview.label.clone(),
            detail: "Selected range".into(),
            source,
        }];
        self.selected = 0;
        self.narrowing = false;
        self.select(0);
    }
    pub(super) fn add(&mut self) {
        self.dirty = true;
        let Some(preview) = self.preview.clone() else {
            return;
        };
        if preview.text.len() != preview.bytes as usize {
            self.status = "Wait for the retained snapshot to load.".into();
            return;
        }
        if self.attached.len() >= ContextSnapshot::MAX_COUNT && self.editing.is_none() {
            self.status = "At most 32 attachments per message.".into();
            return;
        }
        let expected = match &preview.source {
            clackatoo_protocol::ContextRef::File { lines: None, .. }
            | clackatoo_protocol::ContextRef::Diagnostics { .. }
            | clackatoo_protocol::ContextRef::Command { .. }
            | clackatoo_protocol::ContextRef::Project { .. } => None,
            _ => Some(preview.revision.clone()),
        };
        let selection = ContextSelection {
            source: self
                .items
                .get(self.selected)
                .map(|c| &c.source)
                .filter(|s| {
                    matches!(
                        s,
                        clackatoo_protocol::ContextRef::Retained { .. }
                            | clackatoo_protocol::ContextRef::Owned { .. }
                    )
                })
                .cloned()
                .unwrap_or_else(|| preview.source.clone()),
            mode: preview.mode,
            expected,
        };
        let attached = Attached {
            selection,
            preview,
            ready: true,
        };
        if let Some(old) = self.editing.and_then(|index| self.attached.get_mut(index)) {
            *old = attached;
        } else if let Some(old) = self
            .attached
            .iter_mut()
            .find(|a| a.selection.source == attached.selection.source)
        {
            *old = attached;
        } else {
            self.attached.push(attached);
        }
        self.status = "Attached · close the picker to write your message".into();
    }
}

impl ChatView {
    pub(crate) fn context_ref(
        &self,
        chat: clackatoo_protocol::TerminalId,
    ) -> Option<clackatoo_protocol::ContextRef> {
        let turn = u32::try_from(self.node.turns.len().checked_sub(1)?).ok()?;
        Some(clackatoo_protocol::ContextRef::Chat {
            chat,
            first: self.first_turn().saturating_add(turn),
            last: self.first_turn().saturating_add(turn),
            requests: false,
        })
    }
    pub(super) fn restore_context_draft(&mut self, selections: &[ContextSelection]) {
        let mut restored = selections
            .iter()
            .map(|selection| {
                self.context
                    .attached
                    .iter()
                    .find(|a| a.selection == *selection)
                    .cloned()
                    .unwrap_or_else(|| Attached {
                        selection: selection.clone(),
                        ready: true,
                        preview: ContextSnapshot {
                            grant: String::new(),
                            id: String::new(),
                            source: selection.source.clone(),
                            label: format!("{:?}", selection.source),
                            location: String::new(),
                            mode: selection.mode,
                            text: String::new(),
                            bytes: 0,
                            revision: selection.expected.clone().unwrap_or_default(),
                            captured: 0,
                            partial: false,
                            note: "Restored draft selection · preview before sending".into(),
                        },
                    })
            })
            .collect::<Vec<_>>();
        for old in &self.context.attached {
            if !restored.iter().any(|a| a.selection == old.selection) {
                restored.push(old.clone());
            }
        }
        self.context.attached = restored;
        self.context.dirty = true;
    }
    pub(crate) fn attach_context(&mut self, source: clackatoo_protocol::ContextRef, label: String) {
        if !self.context_supported {
            self.say("Update the daemon to attach workspace context.".into());
            return;
        }
        if self.context.attached.len() + self.images.len() >= ContextSnapshot::MAX_COUNT {
            self.say("Attach at most 32 objects.".into());
            return;
        }
        if self
            .context
            .attached
            .iter()
            .any(|a| a.selection.source == source)
        {
            return;
        }
        let selection = ContextSelection {
            source: source.clone(),
            mode: ContextMode::Contents,
            expected: None,
        };
        let preview = ContextSnapshot {
            grant: String::new(),
            bytes: 0,
            id: String::new(),
            source: source.clone(),
            label,
            location: String::new(),
            mode: ContextMode::Contents,
            text: String::new(),
            revision: String::new(),
            captured: 0,
            partial: false,
            note: "Capturing preview…".into(),
        };
        self.context.attached.push(Attached {
            selection: selection.clone(),
            preview,
            ready: false,
        });
        self.context.next += 1;
        let serial = self.context.next;
        self.context.automatic.insert(serial, source);
        self.context
            .requests
            .push((serial, ContextAction::Preview(selection)));
        self.context.dirty = true;
    }
    pub(crate) fn open_context(&mut self) {
        self.context.history.clear();
        self.context.open = true;
        self.context.offset = 0;
        self.context.search();
    }
    pub(crate) fn take_context_requests(&mut self) -> Vec<(u64, ContextAction)> {
        self.context.dirty = false;
        std::mem::take(&mut self.context.requests)
    }
    pub(crate) fn context_answer(&mut self, serial: u64, response: Response) {
        if let Some(source) = self.context.automatic.remove(&serial) {
            if let Some(attached) = self
                .context
                .attached
                .iter_mut()
                .find(|a| a.selection.source == source)
            {
                match response {
                    Response::Context(ContextReply::Snapshot(snapshot)) => {
                        attached.selection.expected = match &source {
                            clackatoo_protocol::ContextRef::File { lines: None, .. } => None,
                            _ => Some(snapshot.revision.clone()),
                        };
                        attached.preview = snapshot;
                        attached.ready = true;
                    }
                    Response::Error(e) => {
                        attached.preview.note = e.to_string();
                        self.context.status = e.to_string();
                    }
                    _ => {
                        attached.preview.note =
                            "Preview was not confirmed. Refresh it before sending.".into();
                    }
                }
            }
            self.context.dirty = true;
            return;
        }
        if serial != self.context.pending {
            return;
        }
        match response {
            Response::Context(ContextReply::Candidates {
                items,
                more,
                notice,
            }) => {
                self.context.items = items;
                self.context.more = more;
                self.context.selected = 0;
                self.context.status = if notice.is_empty() {
                    if more {
                        "100 results · Page Down for more"
                    } else {
                        "Bounded search · Select to preview · Enter attaches"
                    }
                    .into()
                } else {
                    notice
                };
            }
            Response::Context(ContextReply::Snapshot(snapshot)) => {
                for old in &mut self.context.history {
                    if old.id == snapshot.id {
                        *old = snapshot.clone();
                    }
                }
                self.context.preview = Some(snapshot);
                self.context.status = "Enter or Attach adds this context".into();
            }
            Response::Error(e) => self.context.status = e.to_string(),
            _ => self.context.status = "Context was not confirmed. Refresh to retry.".into(),
        }
    }
    pub(super) fn context_prefix(&mut self) -> bool {
        if self.context.open {
            return false;
        }
        let cursor = self.editor.raw_selection().focus().index();
        let text = self.editor.raw_text();
        let Some(before) = text.get(..cursor) else {
            return false;
        };
        let Some(at) = before.rfind('@') else {
            return false;
        };
        if at > 0 && !before[..at].ends_with(char::is_whitespace) {
            return false;
        }
        let rest = &before[at + 1..];
        let Some((prefix, query)) = rest.split_once(':') else {
            return false;
        };
        let kind = match prefix {
            "context" => ContextKind::All,
            "file" => ContextKind::File,
            "symbol" => ContextKind::Symbol,
            "diag" | "diagnostics" => ContextKind::Diagnostics,
            "command" | "block" => ContextKind::Command,
            "change" | "hunk" => ContextKind::Change,
            "chat" => ContextKind::Chat,
            "board" => ContextKind::Board,
            "project" => ContextKind::Project,
            _ => return false,
        };
        let query = query.to_owned();
        let mut text = text.to_owned();
        text.replace_range(at..cursor, "");
        self.editor.set_text(&text);
        self.context.kind = kind;
        self.context.query = query;
        self.open_context();
        true
    }
    pub(super) fn context_key(&mut self, key: &Key) -> bool {
        if !self.context.open {
            return false;
        }
        self.context.dirty = true;
        if self.context.narrowing {
            match key {
                Key::Named(NamedKey::Escape) => self.context.narrowing = false,
                Key::Named(NamedKey::Backspace) => {
                    self.context.range.pop();
                }
                Key::Character(s) if self.context.range.len() + s.len() <= 40 => {
                    self.context.range.push_str(s);
                }
                Key::Named(NamedKey::Enter) => self.context.narrow(),
                _ => {}
            }
            return true;
        }
        match key {
            Key::Named(NamedKey::Escape) => self.context.open = false,
            Key::Named(NamedKey::Tab) => {
                let index = KINDS
                    .iter()
                    .position(|k| *k == self.context.kind)
                    .unwrap_or(0);
                self.context.kind = KINDS[(index + 1) % KINDS.len()];
                self.context.offset = 0;
                self.context.search();
            }
            Key::Named(NamedKey::ArrowDown) => {
                if !self.context.items.is_empty() {
                    self.context.selected =
                        (self.context.selected + 1).min(self.context.items.len() - 1);
                    self.context.preview = None;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.context.selected = self.context.selected.saturating_sub(1);
                self.context.preview = None;
            }
            Key::Named(NamedKey::Enter) => {
                if self.context.preview.is_some() {
                    self.context.add();
                } else {
                    self.context.select(self.context.selected);
                }
            }
            Key::Named(NamedKey::PageDown) if self.context.preview.is_some() => {
                self.context.scroll = (self.context.scroll + 400.0).min(self.context.max_scroll);
            }
            Key::Named(NamedKey::PageUp) if self.context.preview.is_some() => {
                self.context.scroll = (self.context.scroll - 400.0).max(0.0);
            }
            Key::Named(NamedKey::PageDown) if self.context.more => {
                self.context.offset += 100;
                self.context.search();
            }
            Key::Named(NamedKey::PageUp) => {
                self.context.offset = self.context.offset.saturating_sub(100);
                self.context.search();
            }
            Key::Named(NamedKey::Backspace) => {
                self.context.query.pop();
                self.context.offset = 0;
                self.context.search();
            }
            Key::Character(s) if self.context.query.len() + s.len() <= 1024 => {
                self.context.query.push_str(s);
                self.context.offset = 0;
                self.context.search();
            }
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clackatoo_protocol::{ContextRef, MachineId};
    fn snapshot() -> ContextSnapshot {
        ContextSnapshot {
            grant: "grant".into(),
            id: "id".into(),
            source: ContextRef::File {
                path: "src/auth.rs".into(),
                lines: None,
            },
            label: "auth.rs".into(),
            location: "src/auth.rs".into(),
            mode: ContextMode::Contents,
            text: "original".into(),
            bytes: 8,
            revision: "rev".into(),
            captured: 1,
            partial: false,
            note: String::new(),
        }
    }
    #[test]
    fn picker_preserves_owner_and_precise_ranges_when_narrowed() {
        let preview = snapshot();
        let owner = MachineId([5; 16]);
        let mut picker = ContextUi {
            preview: Some(preview.clone()),
            items: vec![ContextCandidate {
                kind: ContextKind::File,
                label: "auth".into(),
                detail: String::new(),
                source: ContextRef::Owned {
                    machine: owner,
                    source: Box::new(preview.source.clone()),
                },
            }],
            ..ContextUi::default()
        };
        picker.add();
        assert!(
            matches!(picker.selections()[0].source,ContextRef::Owned{machine,..} if machine==owner)
        );
        assert!(picker.selections()[0].expected.is_none());
        picker.range = "2-4".into();
        picker.narrow();
        assert!(
            matches!(&picker.requests.last().unwrap().1,ContextAction::Preview(ContextSelection{source:ContextRef::Owned{machine,source},..}) if *machine==owner&&matches!(source.as_ref(),ContextRef::File{lines:Some((2,4)),..}))
        );
    }
    #[test]
    fn obsolete_previews_cannot_replace_the_current_choice_and_all_attachments_are_inspectable() {
        let mut view = ChatView::new("chat".into(), super::super::fixtures::talk(&[]));
        view.context.request(ContextAction::Search {
            kind: ContextKind::All,
            query: "old".into(),
            offset: 0,
        });
        let old = view.context.pending;
        view.context.request(ContextAction::Search {
            kind: ContextKind::All,
            query: "new".into(),
            offset: 0,
        });
        view.context_answer(old, Response::Context(ContextReply::Snapshot(snapshot())));
        assert!(view.context.preview.is_none());
        for i in 0..32 {
            let mut s = snapshot();
            s.source = ContextRef::File {
                path: format!("{i}.rs"),
                lines: None,
            };
            view.context.preview = Some(s);
            view.context.add();
        }
        assert_eq!(view.context.attached.len(), 32);
        view.context.inspect_attached(31);
        assert_eq!(
            view.context.items[0].source,
            ContextRef::File {
                path: "31.rs".into(),
                lines: None
            }
        );
    }
}
