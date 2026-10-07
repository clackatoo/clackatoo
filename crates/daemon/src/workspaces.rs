//! The daemon's workspaces: making and closing tabs and panes, and keeping
//! each tab's layout tidy as they come and go.
//!
//! Requests are checked before anything changes, so a request that fails
//! leaves no terminal behind: the server checks, starts the terminal, then
//! makes the change, which can't fail.

use clackatoo_protocol::{
    Content, Error, Layout, Pane, PaneId, Side, Tab, TabId, TerminalId, Workspace, WorkspaceId,
    Workspaces,
};

/// Every workspace, and the IDs the next ones get.
#[derive(Debug)]
pub(crate) struct WorkspaceState {
    workspaces: Workspaces,
    next_workspace: u32,
    next_tab: u32,
    next_pane: u32,
}

impl WorkspaceState {
    pub(crate) fn new() -> Self {
        Self {
            workspaces: Workspaces::default(),
            next_workspace: 1,
            next_tab: 1,
            next_pane: 1,
        }
    }

    pub(crate) fn get(&self) -> &Workspaces {
        &self.workspaces
    }

    /// Checks that a new workspace can be called `name`, or picks a name
    /// for it.
    pub(crate) fn name_for_new(&self, name: Option<&str>) -> Result<String, Error> {
        match name {
            Some(name) => self.check_name(name, None),
            None => Ok((1..=u32::MAX)
                .map(|n| n.to_string())
                .find(|name| !self.name_taken(name, None))
                .expect("some number is free")),
        }
    }

    /// Makes a workspace called `name`, which [`name_for_new`] checked, with
    /// one tab showing `terminal`.
    ///
    /// [`name_for_new`]: Self::name_for_new
    pub(crate) fn new_workspace(&mut self, name: String, terminal: TerminalId) -> PaneId {
        let id = WorkspaceId(self.next_workspace);
        self.next_workspace += 1;
        let (tab, pane) = self.tab(terminal);
        self.workspaces.list.push(Workspace {
            id,
            name,
            active_tab: tab.id,
            tabs: vec![tab],
        });
        self.workspaces.active = Some(id);
        pane
    }

    /// Checks that `workspace` exists.
    pub(crate) fn check_workspace(&self, workspace: WorkspaceId) -> Result<&Workspace, Error> {
        self.workspaces
            .get(workspace)
            .ok_or(Error::NoSuchWorkspace(workspace))
    }

    /// Adds a tab showing `terminal` to `workspace`, which
    /// [`check_workspace`](Self::check_workspace) checked.
    pub(crate) fn new_tab(&mut self, workspace: WorkspaceId, terminal: TerminalId) -> PaneId {
        let (tab, pane) = self.tab(terminal);
        let workspace = self.workspace_mut(workspace);
        workspace.active_tab = tab.id;
        workspace.tabs.push(tab);
        pane
    }

    /// Checks that `pane` exists, returning the terminal it shows.
    pub(crate) fn check_pane(&self, pane: PaneId) -> Result<TerminalId, Error> {
        let (_, _, pane) = self
            .workspaces
            .find_pane(pane)
            .ok_or(Error::NoSuchPane(pane))?;
        let Content::Terminal(terminal) = pane.content;
        Ok(terminal)
    }

    /// Puts a pane showing `terminal` on `side` of `pane`, which
    /// [`check_pane`](Self::check_pane) checked.
    pub(crate) fn split(&mut self, pane: PaneId, side: Side, terminal: TerminalId) -> PaneId {
        let new = self.pane(terminal);
        let tab = self.tab_mut(pane);
        split(&mut tab.layout, pane, side, new);
        tab.focused = new.id;
        new.id
    }

    /// The focused pane of `workspace`'s active tab.
    pub(crate) fn focused_pane(&self, workspace: WorkspaceId) -> Option<PaneId> {
        let workspace = self.workspaces.get(workspace)?;
        Some(workspace.tab(workspace.active_tab)?.focused)
    }

    /// Records a switch to `pane`.
    pub(crate) fn focus(&mut self, pane: PaneId) -> Result<(), Error> {
        let (workspace, tab, _) = self
            .workspaces
            .find_pane(pane)
            .ok_or(Error::NoSuchPane(pane))?;
        let (workspace, tab) = (workspace.id, tab.id);
        self.workspaces.active = Some(workspace);
        let workspace = self.workspace_mut(workspace);
        workspace.active_tab = tab;
        workspace
            .tabs
            .iter_mut()
            .find(|candidate| candidate.id == tab)
            .expect("the pane's tab")
            .focused = pane;
        Ok(())
    }

    /// The terminals of the tab `tab`'s panes.
    pub(crate) fn tab_terminals(&self, tab: TabId) -> Result<Vec<TerminalId>, Error> {
        let (_, tab) = self.workspaces.find_tab(tab).ok_or(Error::NoSuchTab(tab))?;
        Ok(tab.terminals())
    }

    /// The terminals of every pane in `workspace`.
    pub(crate) fn workspace_terminals(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<TerminalId>, Error> {
        Ok(self.check_workspace(workspace)?.terminals())
    }

    /// Names `tab`, or with an empty name, takes its name away.
    pub(crate) fn rename_tab(&mut self, tab: TabId, name: &str) -> Result<(), Error> {
        let name = name.trim();
        if name.chars().any(char::is_control) {
            return Err(Error::BadName {
                name: name.to_owned(),
                reason: "names can't contain control characters".to_owned(),
            });
        }
        let tab = self
            .workspaces
            .list
            .iter_mut()
            .flat_map(|workspace| &mut workspace.tabs)
            .find(|candidate| candidate.id == tab)
            .ok_or(Error::NoSuchTab(tab))?;
        tab.name = (!name.is_empty()).then(|| name.to_owned());
        Ok(())
    }

    pub(crate) fn rename(&mut self, workspace: WorkspaceId, name: &str) -> Result<(), Error> {
        self.check_workspace(workspace)?;
        let name = self.check_name(name, Some(workspace))?;
        self.workspace_mut(workspace).name = name;
        Ok(())
    }

    /// Takes the pane showing `terminal` out of its tab, if one does,
    /// closing the tab if it was the last pane, and the workspace if that
    /// was its last tab. Returns whether a pane did.
    pub(crate) fn remove_terminal(&mut self, terminal: TerminalId) -> bool {
        let Some(pane) = self.workspaces.pane_showing(terminal) else {
            return false;
        };
        let (workspace, tab, _) = self.workspaces.find_pane(pane).expect("it was just found");
        let (workspace, tab) = (workspace.id, tab.id);
        let tabs = &mut self.workspace_mut(workspace).tabs;
        let index = tabs.iter().position(|t| t.id == tab).expect("its tab");
        if remove(&mut tabs[index], pane) {
            return true;
        }
        // It was the tab's only pane.
        tabs.remove(index);
        if let Some(next) = tabs.get(index.saturating_sub(1)) {
            let next = next.id;
            let workspace = self.workspace_mut(workspace);
            if workspace.active_tab == tab {
                workspace.active_tab = next;
            }
            return true;
        }
        // And the workspace's only tab.
        let list = &mut self.workspaces.list;
        let index = list
            .iter()
            .position(|w| w.id == workspace)
            .expect("its workspace");
        list.remove(index);
        if self.workspaces.active == Some(workspace) {
            self.workspaces.active = list.get(index.saturating_sub(1)).map(|w| w.id);
        }
        true
    }

    fn check_name(&self, name: &str, renaming: Option<WorkspaceId>) -> Result<String, Error> {
        let bad = |reason: &str| Error::BadName {
            name: name.to_owned(),
            reason: reason.to_owned(),
        };
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(bad("it's empty"));
        }
        // `name@host` names a workspace on another machine.
        if trimmed.contains('@') {
            return Err(bad("names can't contain @"));
        }
        if trimmed.chars().any(char::is_control) {
            return Err(bad("names can't contain control characters"));
        }
        if self.name_taken(trimmed, renaming) {
            return Err(Error::NameTaken(trimmed.to_owned()));
        }
        Ok(trimmed.to_owned())
    }

    fn name_taken(&self, name: &str, except: Option<WorkspaceId>) -> bool {
        self.workspaces
            .list
            .iter()
            .any(|workspace| workspace.name == name && Some(workspace.id) != except)
    }

    fn pane(&mut self, terminal: TerminalId) -> Pane {
        let id = PaneId(self.next_pane);
        self.next_pane += 1;
        Pane {
            id,
            content: Content::Terminal(terminal),
        }
    }

    /// A new tab showing `terminal`, and its pane.
    fn tab(&mut self, terminal: TerminalId) -> (Tab, PaneId) {
        let pane = self.pane(terminal);
        let id = TabId(self.next_tab);
        self.next_tab += 1;
        let tab = Tab {
            id,
            layout: Layout::Pane(pane),
            focused: pane.id,
            name: None,
        };
        (tab, pane.id)
    }

    fn workspace_mut(&mut self, id: WorkspaceId) -> &mut Workspace {
        self.workspaces
            .list
            .iter_mut()
            .find(|workspace| workspace.id == id)
            .expect("callers check the workspace exists")
    }

    fn tab_mut(&mut self, pane: PaneId) -> &mut Tab {
        self.workspaces
            .list
            .iter_mut()
            .flat_map(|workspace| &mut workspace.tabs)
            .find(|tab| tab.layout.contains(pane))
            .expect("callers check the pane exists")
    }
}

/// Puts `new` on `side` of the pane `target` in `layout`. In a split along
/// the same axis, it joins that split, so three panes side by side share
/// the space in thirds.
fn split(layout: &mut Layout, target: PaneId, side: Side, new: Pane) {
    let axis = side.axis();
    match layout {
        Layout::Pane(pane) if pane.id == target => {
            let old = Layout::Pane(*pane);
            let new = Layout::Pane(new);
            let parts = if side.before() {
                vec![new, old]
            } else {
                vec![old, new]
            };
            *layout = Layout::Split { axis, parts };
        }
        Layout::Pane(_) => {}
        Layout::Split {
            axis: split_axis,
            parts,
        } => {
            let leaf = parts
                .iter()
                .position(|part| matches!(part, Layout::Pane(pane) if pane.id == target));
            match leaf {
                Some(index) if *split_axis == axis => {
                    let at = if side.before() { index } else { index + 1 };
                    parts.insert(at, Layout::Pane(new));
                }
                _ => {
                    for part in parts {
                        if part.contains(target) {
                            split(part, target, side, new);
                        }
                    }
                }
            }
        }
    }
}

/// Takes `pane` out of `tab`, and focuses the pane before it if it had the
/// focus. Returns `false`, changing nothing, if it's the tab's only pane.
fn remove(tab: &mut Tab, pane: PaneId) -> bool {
    let Layout::Split { .. } = tab.layout else {
        return false;
    };
    let panes = tab.layout.panes();
    let index = panes.iter().position(|p| p.id == pane).expect("in the tab");
    remove_from(&mut tab.layout, pane);
    tidy(&mut tab.layout);
    if tab.focused == pane {
        let panes = tab.layout.panes();
        tab.focused = panes[index.saturating_sub(1).min(panes.len() - 1)].id;
    }
    true
}

fn remove_from(layout: &mut Layout, pane: PaneId) {
    if let Layout::Split { parts, .. } = layout {
        parts.retain(|part| !matches!(part, Layout::Pane(p) if p.id == pane));
        for part in parts {
            remove_from(part, pane);
        }
    }
}

/// Restores [`Layout`]'s rules after a pane was removed: a split left with
/// one part becomes that part, and a split inside another along the same
/// axis gives its parts to it.
fn tidy(layout: &mut Layout) {
    let Layout::Split { axis, parts } = layout else {
        return;
    };
    let axis = *axis;
    for part in parts.iter_mut() {
        tidy(part);
    }
    let mut flat = Vec::with_capacity(parts.len());
    for part in parts.drain(..) {
        match part {
            Layout::Split {
                axis: inner,
                parts: inner_parts,
            } if inner == axis => flat.extend(inner_parts),
            part => flat.push(part),
        }
    }
    if flat.len() == 1 {
        *layout = flat.pop().expect("one part");
    } else {
        *parts = flat;
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::Axis;

    use super::*;

    /// The layout as text: panes by number, `(a | b)` side by side and
    /// `(a / b)` stacked.
    fn shape(layout: &Layout) -> String {
        match layout {
            Layout::Pane(pane) => pane.id.to_string(),
            Layout::Split { axis, parts } => {
                let divider = match axis {
                    Axis::LeftToRight => " | ",
                    Axis::TopToBottom => " / ",
                };
                let parts: Vec<String> = parts.iter().map(shape).collect();
                format!("({})", parts.join(divider))
            }
        }
    }

    fn only_tab(state: &WorkspaceState) -> &Tab {
        &state.get().list[0].tabs[0]
    }

    #[test]
    fn splits_join_splits_along_the_same_axis_and_nest_across_it() {
        let mut state = WorkspaceState::new();
        let name = state.name_for_new(None).unwrap();
        let one = state.new_workspace(name, TerminalId(1));
        let two = state.split(one, Side::Right, TerminalId(2));
        assert_eq!(shape(&only_tab(&state).layout), "(1 | 2)");
        state.split(one, Side::Right, TerminalId(3));
        assert_eq!(shape(&only_tab(&state).layout), "(1 | 3 | 2)");
        state.split(two, Side::Below, TerminalId(4));
        assert_eq!(shape(&only_tab(&state).layout), "(1 | 3 | (2 / 4))");
        state.split(two, Side::Above, TerminalId(5));
        assert_eq!(shape(&only_tab(&state).layout), "(1 | 3 | (5 / 2 / 4))");
        state.split(one, Side::Left, TerminalId(6));
        assert_eq!(shape(&only_tab(&state).layout), "(6 | 1 | 3 | (5 / 2 / 4))");
        assert_eq!(
            only_tab(&state).focused,
            PaneId(6),
            "the new pane is focused"
        );
    }

    #[test]
    fn closing_panes_tidies_the_layout_then_the_tab_then_the_workspace() {
        let mut state = WorkspaceState::new();
        let one = state.new_workspace("work".into(), TerminalId(1));
        let two = state.split(one, Side::Right, TerminalId(2));
        state.split(two, Side::Below, TerminalId(3));
        assert_eq!(shape(&only_tab(&state).layout), "(1 | (2 / 3))");
        state.focus(two).unwrap();

        assert!(state.remove_terminal(TerminalId(2)));
        assert_eq!(shape(&only_tab(&state).layout), "(1 | 3)");
        assert_eq!(only_tab(&state).focused, one, "the pane before it");
        assert!(!state.remove_terminal(TerminalId(2)), "it's gone");

        let workspace = state.get().list[0].id;
        state.new_tab(workspace, TerminalId(4));
        assert_eq!(state.get().list[0].tabs.len(), 2);
        assert!(state.remove_terminal(TerminalId(4)));
        let workspace = &state.get().list[0];
        assert_eq!(workspace.tabs.len(), 1);
        assert_eq!(workspace.active_tab, workspace.tabs[0].id);

        assert!(state.remove_terminal(TerminalId(1)));
        assert!(state.remove_terminal(TerminalId(3)));
        assert!(state.get().list.is_empty());
        assert_eq!(state.get().active, None);
    }

    #[test]
    fn a_split_inside_a_split_along_the_same_axis_is_merged_into_it() {
        let mut state = WorkspaceState::new();
        let one = state.new_workspace("work".into(), TerminalId(1));
        let two = state.split(one, Side::Below, TerminalId(2));
        state.split(two, Side::Right, TerminalId(3));
        state.split(two, Side::Below, TerminalId(4));
        assert_eq!(shape(&only_tab(&state).layout), "(1 / ((2 / 4) | 3))");
        // Without 3, the stack 2 / 4 lands in the outer stack.
        state.remove_terminal(TerminalId(3));
        assert_eq!(shape(&only_tab(&state).layout), "(1 / 2 / 4)");
    }

    #[test]
    fn names_are_unique_and_unnamed_workspaces_get_the_lowest_free_number() {
        let mut state = WorkspaceState::new();
        assert_eq!(state.name_for_new(None).unwrap(), "1");
        state.new_workspace("1".into(), TerminalId(1));
        state.new_workspace("3".into(), TerminalId(2));
        assert_eq!(state.name_for_new(None).unwrap(), "2");
        assert_eq!(state.name_for_new(Some(" peios ")).unwrap(), "peios");
        assert_eq!(
            state.name_for_new(Some("3")),
            Err(Error::NameTaken("3".into()))
        );
        assert!(matches!(
            state.name_for_new(Some("a@b")),
            Err(Error::BadName { .. })
        ));
        assert!(matches!(
            state.name_for_new(Some("  ")),
            Err(Error::BadName { .. })
        ));

        let first = state.get().list[0].id;
        state.rename(first, "1").unwrap();
        assert_eq!(state.rename(first, "3"), Err(Error::NameTaken("3".into())));
        state.rename(first, "clackatoo").unwrap();
        assert_eq!(state.get().list[0].name, "clackatoo");
    }

    #[test]
    fn tabs_can_be_named_and_unnamed() {
        let mut state = WorkspaceState::new();
        state.new_workspace("a".into(), TerminalId(1));
        let tab = state.get().list[0].tabs[0].id;
        state.rename_tab(tab, " logs ").unwrap();
        assert_eq!(state.get().list[0].tabs[0].name.as_deref(), Some("logs"));
        state.rename_tab(tab, "").unwrap();
        assert_eq!(state.get().list[0].tabs[0].name, None);
        assert_eq!(
            state.rename_tab(TabId(9), "x"),
            Err(Error::NoSuchTab(TabId(9)))
        );
        assert_eq!(state.tab_terminals(tab).unwrap(), [TerminalId(1)]);
    }

    #[test]
    fn focus_records_the_tab_pane_and_workspace() {
        let mut state = WorkspaceState::new();
        let one = state.new_workspace("a".into(), TerminalId(1));
        let a = state.get().list[0].id;
        let two = state.new_tab(a, TerminalId(2));
        state.new_workspace("b".into(), TerminalId(3));
        assert_eq!(state.focused_pane(a), Some(two));
        state.focus(one).unwrap();
        assert_eq!(state.get().active, Some(a));
        assert_eq!(state.focused_pane(a), Some(one));
        assert_eq!(state.focus(PaneId(99)), Err(Error::NoSuchPane(PaneId(99))));
    }
}
