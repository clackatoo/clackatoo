//! A quiet badge and a searchable, live list for daemon background work.

use clackatoo_protocol::{Request, Response, WorkAction, WorkItem, WorkState, wire};

use super::{Asked, Desk};
use crate::finder::{Finder, Kind, LocalItem};
use crate::options::Case;

impl Desk {
    pub(crate) fn work_changed(&mut self, items: Vec<WorkItem>) {
        self.work = items;
        if self
            .finder
            .as_ref()
            .is_some_and(|finder| finder.kind() == Kind::Work)
        {
            let items = self.work_items();
            if let Some(finder) = &mut self.finder {
                finder.relist(items);
            }
        }
    }

    pub(crate) fn work_badge(&self) -> Option<(String, bool)> {
        let active = self.work.iter().filter(|item| item.state.active()).count();
        let failed = self
            .work
            .iter()
            .filter(|item| item.state == WorkState::Failed)
            .count();
        if active > 0 {
            Some((
                format!("Working · {active}{}", if failed > 0 { " !" } else { "" }),
                failed > 0,
            ))
        } else if failed > 0 {
            Some((format!("Work ! {failed}"), true))
        } else {
            None
        }
    }

    pub(crate) fn open_work(&mut self) {
        if !self.needs_daemon("background work", wire::WORK) {
            return;
        }
        self.close_finder();
        let mut finder = Finder::local(Kind::Work, self.work_items(), Case::of(&self.options));
        finder.set_note(Some("Ctrl+D: cancel/dismiss".into()));
        self.finder = Some(finder);
        self.request(Request::Work(WorkAction::List), Asked::Work);
    }

    fn work_items(&self) -> Vec<LocalItem> {
        let mut work: Vec<_> = self.work.iter().collect();
        work.sort_by_key(|item| {
            (
                match item.state {
                    WorkState::Running | WorkState::Cancelling => 0,
                    WorkState::Queued => 1,
                    WorkState::Failed => 2,
                    _ => 3,
                },
                std::cmp::Reverse(item.id),
            )
        });
        work.into_iter()
            .map(|item| {
                let state = state_name(item.state);
                let progress = progress(item);
                let scope = item.scope.as_deref().unwrap_or("Daemon");
                let text = format!("{}  {state}{progress}  {scope}", item.title);
                let start = item.title.len() + 2;
                let mut preview = vec![
                    item.title.clone(),
                    format!("Project: {scope}"),
                    format!("Status: {state}{progress}"),
                    String::new(),
                ];
                if !item.detail.is_empty() {
                    preview.push(item.detail.clone());
                }
                if item.state.active() && item.cancellable {
                    preview.extend([
                        String::new(),
                        "Ctrl+D requests cancellation.".into(),
                        "This work may be shared by several chats.".into(),
                        "Stops when the worker next checks.".into(),
                    ]);
                } else if !item.state.active() {
                    preview.extend([String::new(), "Ctrl+D dismisses this outcome.".into()]);
                }
                LocalItem {
                    name: item.id.to_string(),
                    name_len: item.title.len(),
                    text,
                    preview,
                    alert: (item.state == WorkState::Failed)
                        .then_some((start, start + state.len())),
                }
            })
            .collect()
    }

    pub(super) fn work_action(&mut self, path: &str) {
        if self.automating || self.controlled {
            self.refuse("Background work is controlled by the user in this window.".into());
            return;
        }
        let Some(item) = path
            .parse::<u64>()
            .ok()
            .and_then(|id| self.work.iter().find(|item| item.id == id))
        else {
            return;
        };
        let action = if item.state.active() {
            if !item.cancellable {
                return;
            }
            WorkAction::Cancel(item.id)
        } else {
            WorkAction::Dismiss(item.id)
        };
        self.request(Request::Work(action), Asked::Work);
    }

    pub(super) fn work_answered(&mut self, response: Response) -> bool {
        match response {
            Response::Work(items) => self.work_changed(items),
            Response::Error(err) => self.warn(err.to_string()),
            _ => {}
        }
        true
    }
}

fn state_name(state: WorkState) -> &'static str {
    match state {
        WorkState::Queued => "Queued",
        WorkState::Running => "Running",
        WorkState::Cancelling => "Cancelling",
        WorkState::Completed => "Completed",
        WorkState::Failed => "Failed",
        WorkState::Cancelled => "Cancelled",
    }
}

fn progress(item: &WorkItem) -> String {
    match item.total {
        Some(total) => format!(" · {}/{total}", item.completed.min(total)),
        None if item.completed > 0 => format!(" · {} so far", item.completed),
        None => String::new(),
    }
}
