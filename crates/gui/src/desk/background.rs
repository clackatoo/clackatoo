//! Background tabs in the window (CLACKA-121): sending the tab shown, or
//! the focused pane, to the background, where it keeps running out of the
//! tab bar; the icon after the tabs, saying how many of the workspace's are
//! running, red with how one failed until they're looked at; and the list
//! of them, in the finder.
//!
//! The list shows the workspace shown's background tabs first, then the
//! other workspaces', each with its name, how it's getting on and its
//! workspace, and beside them the last screenful of the one picked, as
//! the finder previews a file: asked of the daemon as it's picked, and
//! again now and then while the list is open ([`PREVIEW_AGAIN`]). Opening
//! the list says the workspace's failures were seen.
//!
//! | Key, in the list | Does |
//! |---|---|
//! | Enter | Brings it back as a tab of its workspace, going there |
//! | Ctrl+D | Dismisses it: twice for one still running |

use std::time::{Duration, Instant};

use clackatoo_protocol::{
    BackgroundAction, BackgroundState, Node, Request, Response, TabId, TerminalStatus, Workspace,
    wire,
};

use super::{Asked, Desk};
use crate::chrome::BackgroundIcon;
use crate::command::BackgroundCommand;
use crate::finder::{Finder, Kind, LocalItem};
use crate::options::Case;

/// How often the preview of the background tab picked is asked for again
/// while the list is open: what a server prints shows soon enough.
pub(super) const PREVIEW_AGAIN: Duration = Duration::from_secs(2);

impl Desk {
    /// Carries out `:bg`, `:bgpane` or `:Background`.
    pub(super) fn background_command(&mut self, command: BackgroundCommand) {
        if !self.needs_daemon("background tabs", wire::BACKGROUND) {
            return;
        }
        let Some(shown) = self.shown else { return };
        let action = match command {
            BackgroundCommand::Tab => BackgroundAction::Tab { tab: shown.tab },
            BackgroundCommand::Pane => BackgroundAction::Pane { pane: shown.pane },
            BackgroundCommand::List => return self.open_background_list(),
        };
        self.request(Request::Background(action), Asked::Command);
    }

    /// The icon after the tabs, for the shown workspace's background tabs:
    /// while one is running, or one failed and no window has said it was
    /// seen.
    pub(crate) fn background_icon(&self) -> Option<BackgroundIcon> {
        let workspace = self.workspaces.get(self.shown?.workspace)?;
        let icon = BackgroundIcon {
            running: workspace.background_running(),
            failed: workspace.background_failed(),
        };
        (icon.running > 0 || icon.failed.is_some()).then_some(icon)
    }

    /// Lists the background tabs in the finder: the shown workspace's,
    /// then the others'. Their failures are seen then.
    pub(crate) fn open_background_list(&mut self) {
        if !self.needs_daemon("background tabs", wire::BACKGROUND) {
            return;
        }
        self.close_finder();
        let items = self.background_items();
        if items.is_empty() {
            self.say("there are no background tabs: Ctrl+B b t sends a tab there".into());
            return;
        }
        let case = Case::of(&self.options);
        self.finder = Some(Finder::local(Kind::Background, items, case));
        self.dismissing = None;
        self.background_preview = None;
        self.background_seen.clear();
        self.see_background_failures();
        self.ask_background_preview();
    }

    /// Says the shown workspace's background failures were seen, if it has
    /// any, keeping them for the list to go on saying while it's open.
    fn see_background_failures(&mut self) {
        let Some(shown) = self.shown else { return };
        let Some(workspace) = self.workspaces.get(shown.workspace) else {
            return;
        };
        let failed: Vec<(TabId, i32)> = (workspace.background.iter())
            .filter_map(|background| Some((background.tab.id, background.failed?)))
            .collect();
        if !failed.is_empty() {
            for (tab, code) in failed {
                self.background_seen.retain(|&(seen, _)| seen != tab);
                self.background_seen.push((tab, code));
            }
            let seen = BackgroundAction::Seen {
                workspace: shown.workspace,
            };
            self.request(Request::Background(seen), Asked::Quiet);
        }
    }

    /// What the list of background tabs lists: the shown workspace's, then
    /// the others', each by its tab's number, saying their workspaces.
    fn background_items(&self) -> Vec<LocalItem> {
        let shown = self.shown.map(|shown| shown.workspace);
        let (here, others): (Vec<&Workspace>, Vec<&Workspace>) = (self.workspaces.list.iter())
            .filter(|workspace| !workspace.background.is_empty())
            .partition(|workspace| Some(workspace.id) == shown);
        let mut items = Vec::new();
        for workspace in here.into_iter().chain(others) {
            // The workspace shown's go without saying it.
            let place = if Some(workspace.id) == shown {
                String::new()
            } else {
                self.shown_name(workspace)
            };
            for background in &workspace.background {
                let name = match background.name.trim() {
                    "" => format!("tab {}", background.tab.id),
                    name => name.to_owned(),
                };
                // A failure seen as the list opened still says so in it.
                let seen = (self.background_seen.iter())
                    .find(|&&(tab, _)| tab == background.tab.id)
                    .map(|&(_, code)| code);
                let (state, failed) = state_said(background.state, background.failed.or(seen));
                let mut text = format!("{name}  ");
                let alert = failed.then(|| (text.len(), text.len() + state.len()));
                text.push_str(&state);
                if !place.is_empty() {
                    text.push_str("  ");
                    text.push_str(&place);
                }
                items.push(LocalItem {
                    name: background.tab.id.to_string(),
                    name_len: name.len(),
                    text,
                    preview: Vec::new(),
                    alert,
                });
            }
        }
        items
    }

    /// Lists the background tabs again, as the workspaces changed, if the
    /// list is open: closed if there are none left.
    pub(super) fn relist_background(&mut self) {
        if !self
            .finder
            .as_ref()
            .is_some_and(|finder| finder.kind() == Kind::Background)
        {
            return;
        }
        let items = self.background_items();
        if items.is_empty() {
            self.close_finder();
            return;
        }
        if let Some(finder) = &mut self.finder {
            finder.relist(items);
        }
        // Those that fail while it's open are seen as they do.
        self.see_background_failures();
        self.ask_background_preview();
    }

    /// The background tab picked in the list, if it's open.
    fn picked_background(&mut self) -> Option<TabId> {
        let finder = self.finder.as_mut()?;
        if finder.kind() != Kind::Background {
            return None;
        }
        Some(TabId(finder.picked_path()?.parse().ok()?))
    }

    /// Asks the daemon for the last screenful of the background tab
    /// picked, if it's not the one last asked for.
    pub(super) fn ask_background_preview(&mut self) {
        let Some(tab) = self.picked_background() else {
            return;
        };
        if self
            .background_preview
            .is_some_and(|(asked, _)| asked == tab)
        {
            return;
        }
        self.preview_background(tab);
    }

    /// Asks the daemon for the last screenful of background tab `tab`'s
    /// focused terminal.
    fn preview_background(&mut self, tab: TabId) {
        let Some((_, background)) = self.workspaces.find_background(tab) else {
            return;
        };
        let focused = (background.tab.layout.panes().into_iter())
            .find(|pane| pane.id == background.tab.focused)
            .and_then(|pane| pane.content.terminal());
        let Some(terminal) = focused else { return };
        self.background_preview = Some((tab, Instant::now()));
        self.request(Request::Capture { terminal }, Asked::BackgroundPreview(tab));
    }

    /// When the window must wake for the list's preview to be asked for
    /// again, while it's open.
    pub(crate) fn background_wakes(&self) -> Option<Instant> {
        let (_, asked) = self.background_preview?;
        let open = (self.finder.as_ref()).is_some_and(|finder| finder.kind() == Kind::Background);
        open.then_some(asked + PREVIEW_AGAIN)
    }

    /// Asks again for the list's preview, if it's open and it's time to.
    /// Returns whether anything was asked.
    pub(crate) fn tick_background(&mut self, now: Instant) -> bool {
        if self.background_wakes().is_none_or(|due| due > now) {
            return false;
        }
        let Some(tab) = self.picked_background() else {
            return false;
        };
        self.preview_background(tab);
        true
    }

    /// Takes in the daemon's answer, the screen of background tab `tab`'s
    /// focused terminal, as the list's preview of it. Returns whether to
    /// draw again.
    pub(super) fn background_preview_came(&mut self, tab: TabId, response: Response) -> bool {
        let lines = match response {
            Response::View(view) => match view.root {
                Node::Grid(grid) => {
                    let mut lines: Vec<String> = (grid.lines.iter())
                        .map(|line| {
                            let text: String =
                                line.runs.iter().map(|run| run.text.as_str()).collect();
                            text.trim_end().to_owned()
                        })
                        .collect();
                    // Blank lines below the last written mean nothing.
                    while lines.last().is_some_and(String::is_empty) {
                        lines.pop();
                    }
                    lines
                }
                Node::Chat(_) | Node::Other => return false,
            },
            Response::Error(err) => vec![format!("its screen can't be shown: {err}")],
            _ => return false,
        };
        let Some(finder) = &mut self.finder else {
            return false;
        };
        if finder.kind() != Kind::Background {
            return false;
        }
        finder.set_local_preview(&tab.to_string(), lines);
        true
    }

    /// Brings back the background tab the list's `path` names, as a tab of
    /// its workspace, going there.
    pub(super) fn bring_background(&mut self, path: &str) {
        let Ok(tab) = path.parse().map(TabId) else {
            return;
        };
        self.request(
            Request::Background(BackgroundAction::Bring { tab }),
            Asked::Pane,
        );
    }

    /// Dismisses the background tab the list's `path` names: at once if
    /// it's not running, or else on the second time of asking.
    pub(super) fn dismiss_background(&mut self, path: &str) {
        let Ok(tab) = path.parse().map(TabId) else {
            return;
        };
        let Some((_, background)) = self.workspaces.find_background(tab) else {
            return;
        };
        let running = !background.state.ended();
        if running && self.dismissing != Some(tab) {
            self.dismissing = Some(tab);
            let name = match background.name.trim() {
                "" => format!("tab {tab}"),
                name => format!("‘{name}’"),
            };
            self.say(format!("{name} is still running: Ctrl+D again ends it"));
            return;
        }
        self.dismissing = None;
        self.request(
            Request::Background(BackgroundAction::Dismiss { tab }),
            Asked::Command,
        );
    }
}

/// How a background tab getting on as `state` says it is, with `failed`,
/// a failure unseen: and whether that's something gone wrong.
fn state_said(state: BackgroundState, failed: Option<i32>) -> (String, bool) {
    match (state, failed) {
        (BackgroundState::Ended(TerminalStatus::Exited(0) | TerminalStatus::Running), _) => {
            ("exited".to_owned(), false)
        }
        (BackgroundState::Ended(TerminalStatus::Exited(code)), _) => {
            (format!("exited {code}"), true)
        }
        (BackgroundState::Ended(TerminalStatus::Killed(signal)), _) => {
            (format!("killed by signal {signal}"), true)
        }
        (BackgroundState::Running, Some(code)) => (format!("running · failed {code}"), true),
        (BackgroundState::Idle, Some(code)) => (format!("idle · failed {code}"), true),
        (BackgroundState::Running, None) => ("running".to_owned(), false),
        (BackgroundState::Idle, None) => ("idle".to_owned(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_background_tab_says_how_it_gets_on_and_what_went_wrong() {
        let said = |state, failed| state_said(state, failed);
        assert_eq!(
            said(BackgroundState::Running, None),
            ("running".into(), false)
        );
        assert_eq!(
            said(BackgroundState::Idle, Some(2)),
            ("idle · failed 2".into(), true)
        );
        let ended = |status| BackgroundState::Ended(status);
        assert_eq!(
            said(ended(TerminalStatus::Exited(0)), None),
            ("exited".into(), false)
        );
        assert_eq!(
            said(ended(TerminalStatus::Exited(3)), Some(3)),
            ("exited 3".into(), true)
        );
        assert_eq!(
            said(ended(TerminalStatus::Killed(9)), Some(137)),
            ("killed by signal 9".into(), true)
        );
    }
}
