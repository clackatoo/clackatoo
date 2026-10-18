//! The daemon's workspaces: making and closing tabs and panes, and keeping
//! each tab's layout tidy as they come and go.
//!
//! Requests are checked before anything changes, so a request that fails
//! leaves no terminal behind: the server checks, starts the terminal, then
//! makes the change, which can't fail.

use clackatoo_protocol::{
    Axis, BoardId, BufferId, Content, Error, FilesId, Layout, MAX_NESTING, Pane, PaneId, Restored,
    ReviewId, Side, Tab, TabId, TerminalId, Workspace, WorkspaceId, Workspaces,
};

use crate::handover::{self, HandoverError};

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

    /// No workspaces yet, on a machine whose user's home is `home`.
    pub(crate) fn with_home(home: &std::path::Path) -> Self {
        let mut state = Self::new();
        state.set_home(home);
        state
    }

    /// Tells windows the user's home is `home`, as they write paths under
    /// it from `~`. A home that isn't UTF-8 isn't told, nor is `/`, which
    /// the daemon starts terminals in when `$HOME` isn't set: a window
    /// would name every terminal at the root `~`.
    pub(crate) fn set_home(&mut self, home: &std::path::Path) {
        self.workspaces.home = (home.to_str())
            .filter(|home| !home.trim_end_matches('/').is_empty())
            .map(str::to_owned);
    }

    /// Tells windows which daemon this is.
    pub(crate) fn set_machine(&mut self, machine: clackatoo_protocol::Machine) {
        self.workspaces.machine = Some(machine);
    }

    /// The workspaces, for the next build.
    pub(crate) fn hand_over(&self) -> handover::Workspaces {
        fn layout(node: &Layout) -> handover::Layout {
            match node {
                Layout::Pane(pane) => match pane.content {
                    Content::Terminal(terminal) => handover::Layout::Pane {
                        id: pane.id.0,
                        terminal: terminal.0,
                    },
                    Content::Buffer(buffer) => handover::Layout::Buffer {
                        id: pane.id.0,
                        buffer: buffer.0,
                    },
                    Content::Files(files) => handover::Layout::Files {
                        id: pane.id.0,
                        files: files.0,
                    },
                    Content::Board(board) => handover::Layout::Board {
                        id: pane.id.0,
                        board: board.0,
                    },
                    Content::Review(review) => handover::Layout::Review {
                        id: pane.id.0,
                        review: review.0,
                    },
                    Content::Browser(chat) => handover::Layout::Browser {
                        id: pane.id.0,
                        chat: chat.0,
                    },
                    Content::Folder(files) => handover::Layout::Folder {
                        id: pane.id.0,
                        files: files.0,
                    },
                    // Only what a newer daemon sends a client is Other:
                    // no request makes one, so no daemon's workspaces hold
                    // one.
                    Content::Other { kind } => {
                        unreachable!(
                            "the daemon's workspaces hold a {kind} pane, of no kind it has"
                        )
                    }
                },
                Layout::Split {
                    axis,
                    parts,
                    shares,
                } => handover::Layout::Split {
                    left_to_right: *axis == Axis::LeftToRight,
                    parts: parts.iter().map(layout).collect(),
                    shares: shares.clone(),
                },
            }
        }
        handover::Workspaces {
            list: (self.workspaces.list.iter())
                .map(|workspace| handover::Workspace {
                    id: workspace.id.0,
                    name: workspace.name.clone(),
                    tabs: (workspace.tabs.iter())
                        .map(|tab| handover::Tab {
                            id: tab.id.0,
                            layout: layout(&tab.layout),
                            focused: tab.focused.0,
                            name: tab.name.clone(),
                        })
                        .collect(),
                    active_tab: workspace.active_tab.0,
                })
                .collect(),
            active: self.workspaces.active.map(|id| id.0),
            next_workspace: self.next_workspace,
            next_tab: self.next_tab,
            next_pane: self.next_pane,
        }
    }

    /// The workspaces the last build handed over.
    pub(crate) fn resume(handed: &handover::Workspaces) -> Result<Self, HandoverError> {
        fn layout(node: &handover::Layout) -> Result<Layout, HandoverError> {
            let handover::Layout::Split {
                left_to_right,
                parts,
                shares,
            } = node
            else {
                return handed_pane(node)
                    .map(Layout::Pane)
                    .ok_or(HandoverError::Damaged("a pane that isn't one"));
            };
            if parts.len() < 2 {
                return Err(HandoverError::Damaged("a split has fewer than two parts"));
            }
            if shares.len() != parts.len() || shares.contains(&0) {
                return Err(HandoverError::Damaged(
                    "a split's shares don't fit its parts",
                ));
            }
            Ok(Layout::Split {
                axis: if *left_to_right {
                    Axis::LeftToRight
                } else {
                    Axis::TopToBottom
                },
                parts: parts.iter().map(layout).collect::<Result<_, _>>()?,
                shares: shares.clone(),
            })
        }
        let mut list = Vec::new();
        for workspace in &handed.list {
            let mut tabs = Vec::new();
            for tab in &workspace.tabs {
                let mut tab = Tab {
                    id: TabId(tab.id),
                    layout: layout(&tab.layout)?,
                    focused: PaneId(tab.focused),
                    name: tab.name.clone(),
                };
                // Builds before splits had a limit could have made one
                // deeper, which no window could read.
                if tab.layout.depth() > MAX_NESTING {
                    log::warn!(
                        "tab {}: its splits were flattened to a readable depth",
                        tab.id
                    );
                    within(&mut tab.layout, MAX_NESTING);
                    tidy(&mut tab.layout);
                }
                if !tab.layout.panes().iter().any(|pane| pane.id == tab.focused) {
                    return Err(HandoverError::Damaged("a tab's focused pane isn't in it"));
                }
                tabs.push(tab);
            }
            let active_tab = TabId(workspace.active_tab);
            if !tabs.iter().any(|tab| tab.id == active_tab) {
                return Err(HandoverError::Damaged(
                    "a workspace's active tab isn't in it",
                ));
            }
            list.push(Workspace {
                id: WorkspaceId(workspace.id),
                name: workspace.name.clone(),
                tabs,
                active_tab,
                home: None,
            });
        }
        let active = handed.active.map(WorkspaceId);
        if active.is_some_and(|active| !list.iter().any(|workspace| workspace.id == active)) {
            return Err(HandoverError::Damaged("the active workspace is missing"));
        }
        Ok(Self {
            workspaces: Workspaces {
                list,
                active,
                home: None,
                restored: Vec::new(),
                machine: None,
            },
            next_workspace: handed.next_workspace,
            next_tab: handed.next_tab,
            next_pane: handed.next_pane,
        })
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
    /// one tab showing `content`.
    ///
    /// [`name_for_new`]: Self::name_for_new
    pub(crate) fn new_workspace(&mut self, name: String, content: impl Into<Content>) -> PaneId {
        let id = WorkspaceId(self.next_workspace);
        self.next_workspace += 1;
        let (tab, pane) = self.tab(content.into());
        self.workspaces.list.push(Workspace {
            id,
            name,
            active_tab: tab.id,
            tabs: vec![tab],
            home: None,
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

    /// Adds a tab showing `content` to `workspace`, which
    /// [`check_workspace`](Self::check_workspace) checked.
    pub(crate) fn new_tab(
        &mut self,
        workspace: WorkspaceId,
        content: impl Into<Content>,
    ) -> PaneId {
        let (tab, pane) = self.tab(content.into());
        let workspace = self.workspace_mut(workspace);
        workspace.active_tab = tab.id;
        workspace.tabs.push(tab);
        pane
    }

    /// Checks that `pane` exists, returning what it shows.
    pub(crate) fn check_pane(&self, pane: PaneId) -> Result<Content, Error> {
        let (_, _, pane) = self
            .workspaces
            .find_pane(pane)
            .ok_or(Error::NoSuchPane(pane))?;
        Ok(pane.content)
    }

    /// Checks that `pane` exists and can be split on `side` without its
    /// tab's splits going deeper than a window can read.
    pub(crate) fn check_split(&self, pane: PaneId, side: Side) -> Result<(), Error> {
        let (_, tab, _) = (self.workspaces.find_pane(pane)).ok_or(Error::NoSuchPane(pane))?;
        let mut layout = tab.layout.clone();
        split(&mut layout, pane, side, placeholder());
        fits(&layout)
    }

    /// Checks that `tab` exists and can be split along `side` without its
    /// splits going deeper than a window can read.
    pub(crate) fn check_split_tab(&self, tab: TabId, side: Side) -> Result<(), Error> {
        let (_, tab) = self.workspaces.find_tab(tab).ok_or(Error::NoSuchTab(tab))?;
        let mut layout = tab.layout.clone();
        split_edge(&mut layout, side, placeholder(), None);
        fits(&layout)
    }

    /// Puts a pane showing `content` on `side` of `pane`, which
    /// [`check_split`](Self::check_split) checked.
    pub(crate) fn split(
        &mut self,
        pane: PaneId,
        side: Side,
        content: impl Into<Content>,
    ) -> PaneId {
        let new = self.pane(content.into());
        let tab = self.tab_mut(pane);
        split(&mut tab.layout, pane, side, new);
        tab.focused = new.id;
        new.id
    }

    /// Puts a pane showing `content` along `side` of `tab`, which
    /// [`check_split_tab`](Self::check_split_tab) checked, as long as the tab, with
    /// `share` thousandths of its space, or as much as each part beside it
    /// if `None`.
    pub(crate) fn split_tab(
        &mut self,
        tab: TabId,
        side: Side,
        content: impl Into<Content>,
        share: Option<u16>,
    ) -> PaneId {
        let new = self.pane(content.into());
        let tab = (self.workspaces.list.iter_mut())
            .flat_map(|workspace| &mut workspace.tabs)
            .find(|candidate| candidate.id == tab)
            .expect("the tab was checked");
        split_edge(&mut tab.layout, side, new, share);
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

    /// The panes of the tab `tab`.
    pub(crate) fn tab_panes(&self, tab: TabId) -> Result<Vec<Pane>, Error> {
        let (_, tab) = self.workspaces.find_tab(tab).ok_or(Error::NoSuchTab(tab))?;
        Ok(tab.layout.panes())
    }

    /// Every pane in `workspace`.
    pub(crate) fn workspace_panes(&self, workspace: WorkspaceId) -> Result<Vec<Pane>, Error> {
        let workspace = self.check_workspace(workspace)?;
        Ok((workspace.tabs.iter())
            .flat_map(|tab| tab.layout.panes())
            .collect())
    }

    /// Shows `content` in `pane`, returning what it showed.
    pub(crate) fn set_content(&mut self, pane: PaneId, content: Content) -> Result<Content, Error> {
        let found = (self.workspaces.list.iter_mut())
            .flat_map(|workspace| &mut workspace.tabs)
            .find_map(|tab| pane_mut(&mut tab.layout, pane))
            .ok_or(Error::NoSuchPane(pane))?;
        let old = std::mem::replace(&mut found.content, content);
        // What it had to show for a restart was for what it showed.
        self.workspaces
            .restored
            .retain(|restored| restored.pane != pane);
        Ok(old)
    }

    /// Has every pane showing buffer `old` show buffer `new`. Returns
    /// whether any did.
    pub(crate) fn show_instead(&mut self, old: BufferId, new: BufferId) -> bool {
        fn swap(layout: &mut Layout, old: BufferId, new: BufferId) -> bool {
            match layout {
                Layout::Pane(pane) if pane.content == Content::Buffer(old) => {
                    pane.content = Content::Buffer(new);
                    true
                }
                Layout::Pane(_) => false,
                Layout::Split { parts, .. } => (parts.iter_mut())
                    .map(|part| swap(part, old, new))
                    .fold(false, |any, swapped| any | swapped),
            }
        }
        (self.workspaces.list.iter_mut())
            .flat_map(|workspace| &mut workspace.tabs)
            .map(|tab| swap(&mut tab.layout, old, new))
            .fold(false, |any, swapped| any | swapped)
    }

    /// Whether any pane shows `buffer`.
    pub(crate) fn shows(&self, buffer: BufferId) -> bool {
        (self.workspaces.list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .any(|pane| pane.content == Content::Buffer(buffer))
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

    /// Shares the split at `path` in `tab` anew: see
    /// [`Request::ResizeSplit`](clackatoo_protocol::Request::ResizeSplit).
    pub(crate) fn resize_split(
        &mut self,
        tab: TabId,
        path: &[u16],
        new_shares: &[u32],
    ) -> Result<(), Error> {
        if new_shares.contains(&0) {
            return Err(Error::EmptyShare);
        }
        let layout = &mut self
            .workspaces
            .list
            .iter_mut()
            .flat_map(|workspace| &mut workspace.tabs)
            .find(|candidate| candidate.id == tab)
            .ok_or(Error::NoSuchTab(tab))?
            .layout;
        match layout.at_mut(path) {
            Some(Layout::Split { parts, shares, .. }) if parts.len() == new_shares.len() => {
                let wide: Vec<u64> = new_shares.iter().map(|&share| u64::from(share)).collect();
                *shares = normalized(&wide);
                Ok(())
            }
            _ => Err(Error::NoSuchSplit(tab)),
        }
    }

    pub(crate) fn rename(&mut self, workspace: WorkspaceId, name: &str) -> Result<(), Error> {
        self.check_workspace(workspace)?;
        let name = self.check_name(name, Some(workspace))?;
        self.workspace_mut(workspace).name = name;
        Ok(())
    }

    /// Takes the pane showing `terminal` out of its tab, if one does, as
    /// [`remove_pane`](Self::remove_pane) does. Returns whether a pane did.
    pub(crate) fn remove_terminal(&mut self, terminal: TerminalId) -> bool {
        match self.workspaces.pane_showing(terminal) {
            Some(pane) => self.remove_pane(pane),
            None => false,
        }
    }

    /// Takes `pane` out of its tab, closing the tab if it was the last
    /// pane, and the workspace if that was its last tab. Returns whether
    /// there was such a pane.
    pub(crate) fn remove_pane(&mut self, pane: PaneId) -> bool {
        let Some((workspace, tab, _)) = self.workspaces.find_pane(pane) else {
            return false;
        };
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

    /// A pane showing `content`, with an ID of its own, to put in a layout
    /// made for [`add_restored`](Self::add_restored).
    pub(crate) fn fresh_pane(&mut self, content: Content) -> Pane {
        self.pane(content)
    }

    /// Adds a workspace made again from the workspaces kept from the last
    /// run of the daemon, called `name` unless that's taken or no name,
    /// with `tabs`: each one's name, its layout of panes from
    /// [`fresh_pane`](Self::fresh_pane), which it tidies, and its focused
    /// pane, or else its first. Tab `active_tab`, by its place, is shown,
    /// or the first if there's no such tab. `tabs` mustn't be empty.
    pub(crate) fn add_restored(
        &mut self,
        name: &str,
        tabs: Vec<(Option<String>, Layout, Option<PaneId>)>,
        active_tab: usize,
    ) -> WorkspaceId {
        let name = (self.name_for_new(Some(name)))
            .or_else(|_| self.name_for_new(None))
            .expect("some number is free");
        let tabs: Vec<Tab> = (tabs.into_iter())
            .map(|(name, mut layout, focused)| {
                // Shares are kept as they were unless the layout breaks
                // its rules, which tidying puts right.
                if untidy(&layout) {
                    tidy(&mut layout);
                }
                if layout.depth() > MAX_NESTING {
                    within(&mut layout, MAX_NESTING);
                    tidy(&mut layout);
                }
                let id = TabId(self.next_tab);
                self.next_tab += 1;
                Tab {
                    id,
                    focused: (focused.filter(|&focused| layout.contains(focused)))
                        .unwrap_or_else(|| layout.panes()[0].id),
                    layout,
                    name: name.filter(|name| {
                        !name.trim().is_empty() && !name.chars().any(char::is_control)
                    }),
                }
            })
            .collect();
        let id = WorkspaceId(self.next_workspace);
        self.next_workspace += 1;
        self.workspaces.list.push(Workspace {
            id,
            name,
            active_tab: tabs.get(active_tab).unwrap_or(&tabs[0]).id,
            tabs,
            home: None,
        });
        id
    }

    /// Makes `workspace` the one last switched to, for new windows.
    pub(crate) fn set_active(&mut self, workspace: WorkspaceId) {
        if self.workspaces.get(workspace).is_some() {
            self.workspaces.active = Some(workspace);
        }
    }

    /// Sets what panes made again as the daemon started have to show for
    /// it (see [`Workspaces::restored`](clackatoo_protocol::Workspaces)).
    pub(crate) fn set_restored(&mut self, restored: Vec<Restored>) {
        self.workspaces.restored = restored;
    }

    /// What `pane`, made again as the daemon started, still has to show
    /// for it, if anything.
    pub(crate) fn restored(&self, pane: PaneId) -> Option<&Restored> {
        (self.workspaces.restored.iter()).find(|restored| restored.pane == pane)
    }

    /// Forgets the command `pane`'s terminal was running, which was to be
    /// offered to run again. Returns whether there was one.
    pub(crate) fn forget_ran(&mut self, pane: PaneId) -> bool {
        self.forget_restored(pane, |restored| restored.ran.take().is_some())
    }

    /// Forgets where editor `pane` was. Returns whether that was kept.
    pub(crate) fn forget_place(&mut self, pane: PaneId) -> bool {
        self.forget_restored(pane, |restored| restored.place.take().is_some())
    }

    /// Takes something away from what `pane` has to show for a restart,
    /// with `take`, which says whether there was that, and forgets the
    /// pane once it has nothing left. Returns what `take` said.
    fn forget_restored(&mut self, pane: PaneId, take: impl FnOnce(&mut Restored) -> bool) -> bool {
        let list = &mut self.workspaces.restored;
        let Some(index) = list.iter().position(|restored| restored.pane == pane) else {
            return false;
        };
        let taken = take(&mut list[index]);
        if list[index].ran.is_none() && list[index].place.is_none() {
            list.remove(index);
        }
        taken
    }

    /// Forgets what panes that have closed had to show for a restart, and
    /// what panes have that's no longer for what they show: a command to
    /// run again only in a terminal, a place only in an editor.
    pub(crate) fn sweep_restored(&mut self) {
        if self.workspaces.restored.is_empty() {
            return;
        }
        let panes: Vec<Pane> = (self.workspaces.list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .collect();
        self.workspaces.restored.retain_mut(|restored| {
            let content = panes.iter().find(|pane| pane.id == restored.pane);
            let content = content.map(|pane| pane.content);
            if !matches!(content, Some(Content::Terminal(_))) {
                restored.ran = None;
            }
            if !matches!(content, Some(Content::Buffer(_))) {
                restored.place = None;
            }
            restored.ran.is_some() || restored.place.is_some()
        });
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

    /// A workspace's number no workspace has had, for one from another
    /// machine, shown among these: numbered as this daemon's own are, so
    /// no two are ever the same. `None` once the numbers have run out.
    pub(crate) fn fresh_workspace(&mut self) -> Option<WorkspaceId> {
        let id = WorkspaceId(self.next_workspace);
        self.next_workspace = self.next_workspace.checked_add(1)?;
        Some(id)
    }

    /// A tab's number no tab has had, as [`fresh_workspace`](Self::fresh_workspace).
    pub(crate) fn fresh_tab(&mut self) -> Option<TabId> {
        let id = TabId(self.next_tab);
        self.next_tab = self.next_tab.checked_add(1)?;
        Some(id)
    }

    /// A pane's number no pane has had, as [`fresh_workspace`](Self::fresh_workspace).
    pub(crate) fn fresh_pane_id(&mut self) -> Option<PaneId> {
        let id = PaneId(self.next_pane);
        self.next_pane = self.next_pane.checked_add(1)?;
        Some(id)
    }

    /// The numbers the next workspace, tab and pane get.
    pub(crate) fn next_numbers(&self) -> (u32, u32, u32) {
        (self.next_workspace, self.next_tab, self.next_pane)
    }

    fn pane(&mut self, content: Content) -> Pane {
        let id = PaneId(self.next_pane);
        self.next_pane += 1;
        Pane {
            id,
            content,
            home: None,
        }
    }

    /// A new tab showing `content`, and its pane.
    fn tab(&mut self, content: Content) -> (Tab, PaneId) {
        let pane = self.pane(content);
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

/// The pane `node` is, as handed over, if it's a pane rather than a split.
fn handed_pane(node: &handover::Layout) -> Option<Pane> {
    let (id, content) = match *node {
        handover::Layout::Pane { id, terminal } => (id, Content::Terminal(TerminalId(terminal))),
        handover::Layout::Buffer { id, buffer } => (id, Content::Buffer(BufferId(buffer))),
        handover::Layout::Files { id, files } => (id, Content::Files(FilesId(files))),
        handover::Layout::Board { id, board } => (id, Content::Board(BoardId(board))),
        handover::Layout::Review { id, review } => (id, Content::Review(ReviewId(review))),
        handover::Layout::Browser { id, chat } => (id, Content::Browser(TerminalId(chat))),
        handover::Layout::Folder { id, files } => (id, Content::Folder(FilesId(files))),
        handover::Layout::Split { .. } => return None,
    };
    Some(Pane {
        id: PaneId(id),
        content,
        home: None,
    })
}

/// A pane standing in for one about to be made, to try a split with.
fn placeholder() -> Pane {
    Pane {
        id: PaneId(u32::MAX),
        content: Content::Terminal(TerminalId(u32::MAX)),
        home: None,
    }
}

/// Refuses `layout` if its splits go deeper than [`MAX_NESTING`], which a
/// window refuses to read: every window would be locked out, and a
/// handover would carry it on.
fn fits(layout: &Layout) -> Result<(), Error> {
    if layout.depth() <= MAX_NESTING {
        return Ok(());
    }
    // The protocol has no error of its own for this, and one would change
    // it: this one is only its words.
    Err(Error::Review(format!(
        "The tab's splits are nested {MAX_NESTING} deep, as deep as they can go: split \
         another way, or close a pane."
    )))
}

/// Keeps `layout` within `room` splits deep: a split at the limit takes
/// every pane below it as its own parts, in equal shares.
fn within(layout: &mut Layout, room: u32) {
    let deeper = layout.depth() > 1;
    let Layout::Split { parts, shares, .. } = layout else {
        return;
    };
    if room > 1 {
        for part in parts {
            within(part, room - 1);
        }
    } else if deeper {
        let panes: Vec<Layout> = (parts.iter())
            .flat_map(Layout::panes)
            .map(Layout::Pane)
            .collect();
        *shares = vec![1; panes.len()];
        *parts = panes;
    }
}

/// The most a split's shares add up to, kept well inside `u32`.
const SHARES_TOTAL: u64 = 1 << 20;

/// `shares` in proportion, adding up to at most [`SHARES_TOTAL`], none 0.
fn normalized(shares: &[u64]) -> Vec<u32> {
    let total: u64 = shares.iter().sum();
    shares
        .iter()
        .map(|&share| {
            let share = if total > SHARES_TOTAL {
                share * SHARES_TOTAL / total
            } else {
                share
            };
            u32::try_from(share.max(1)).expect("at most SHARES_TOTAL")
        })
        .collect()
}

/// Puts `new` on `side` of the pane `target` in `layout`. In a split along
/// the same axis, it joins that split with the average share, so three
/// panes side by side that were never resized share the space in thirds.
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
            *layout = Layout::Split {
                axis,
                parts,
                shares: vec![1, 1],
            };
        }
        Layout::Pane(_) => {}
        Layout::Split {
            axis: split_axis,
            parts,
            shares,
        } => {
            let leaf = parts
                .iter()
                .position(|part| matches!(part, Layout::Pane(pane) if pane.id == target));
            match leaf {
                Some(index) if *split_axis == axis => {
                    let at = if side.before() { index } else { index + 1 };
                    let average = shares.iter().map(|&share| u64::from(share)).sum::<u64>()
                        / shares.len() as u64;
                    parts.insert(at, Layout::Pane(new));
                    let mut wide: Vec<u64> = shares.iter().map(|&share| u64::from(share)).collect();
                    wide.insert(at, average);
                    *shares = normalized(&wide);
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

/// Puts `new` along `side` of the whole of `layout`, with `share`
/// thousandths of its space, or the average share if `None`. In a split
/// along the same axis, it joins that split as its first or last part.
fn split_edge(layout: &mut Layout, side: Side, new: Pane, share: Option<u16>) {
    let axis = side.axis();
    let new = Layout::Pane(new);
    if let Layout::Split {
        axis: split_axis,
        parts,
        shares,
    } = layout
        && *split_axis == axis
    {
        // In thousandths of what they were, so a small share of a split
        // whose shares are small, such as 1 and 1, isn't rounded away.
        let wide: Vec<u64> = shares
            .iter()
            .map(|&share| u64::from(share) * 1000)
            .collect();
        let total: u64 = wide.iter().sum();
        let added = match share {
            // The others keep their part of what's left.
            Some(share) => {
                let share = u64::from(share.clamp(1, 999));
                total * share / (1000 - share)
            }
            None => total / wide.len() as u64,
        };
        let at = if side.before() { 0 } else { parts.len() };
        parts.insert(at, new);
        let mut wide = wide;
        wide.insert(at, added);
        *shares = normalized(&wide);
        return;
    }
    let old = std::mem::replace(
        layout,
        Layout::Pane(Pane {
            id: PaneId(0),
            content: Content::Terminal(TerminalId(0)),
            home: None,
        }),
    );
    let (parts, shares) = match share.map(|share| u32::from(share.clamp(1, 999))) {
        Some(share) if side.before() => (vec![new, old], vec![share, 1000 - share]),
        Some(share) => (vec![old, new], vec![1000 - share, share]),
        None if side.before() => (vec![new, old], vec![1, 1]),
        None => (vec![old, new], vec![1, 1]),
    };
    *layout = Layout::Split {
        axis,
        parts,
        shares,
    };
}

/// `pane`, if it's in `layout`.
fn pane_mut(layout: &mut Layout, pane: PaneId) -> Option<&mut Pane> {
    match layout {
        Layout::Pane(found) => (found.id == pane).then_some(found),
        Layout::Split { parts, .. } => parts.iter_mut().find_map(|part| pane_mut(part, pane)),
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
    if let Layout::Split { parts, shares, .. } = layout {
        if let Some(index) = parts
            .iter()
            .position(|part| matches!(part, Layout::Pane(p) if p.id == pane))
        {
            parts.remove(index);
            shares.remove(index);
        }
        for part in parts {
            remove_from(part, pane);
        }
    }
}

/// Whether `layout` breaks [`Layout`]'s rules: a split with fewer than two
/// parts, shares that don't fit them, or a split inside another along the
/// same axis.
fn untidy(layout: &Layout) -> bool {
    let Layout::Split {
        axis,
        parts,
        shares,
    } = layout
    else {
        return false;
    };
    parts.len() < 2
        || shares.len() != parts.len()
        || shares.contains(&0)
        || (parts.iter()).any(|part| {
            matches!(part, Layout::Split { axis: inner, .. } if inner == axis) || untidy(part)
        })
}

/// Restores [`Layout`]'s rules after a pane was removed: a split left with
/// one part becomes that part, and a split inside another along the same
/// axis gives its parts to it.
fn tidy(layout: &mut Layout) {
    let Layout::Split {
        axis,
        parts,
        shares,
    } = layout
    else {
        return;
    };
    let axis = *axis;
    for part in parts.iter_mut() {
        tidy(part);
    }
    // A split given to this one shares out the space its part had.
    let mut flat = Vec::with_capacity(parts.len());
    let mut flat_shares = Vec::with_capacity(parts.len());
    for (part, share) in parts.drain(..).zip(shares.drain(..)) {
        let share = u64::from(share) * SHARES_TOTAL;
        match part {
            Layout::Split {
                axis: inner,
                parts: inner_parts,
                shares: inner_shares,
            } if inner == axis => {
                let total: u64 = inner_shares.iter().map(|&inner| u64::from(inner)).sum();
                flat.extend(inner_parts);
                flat_shares.extend(
                    inner_shares
                        .iter()
                        // At most 2^40 times 2^20: well inside u64.
                        .map(|&inner| share * u64::from(inner) / total.max(1)),
                );
            }
            part => {
                flat.push(part);
                flat_shares.push(share);
            }
        }
    }
    if flat.len() == 1 {
        *layout = flat.pop().expect("one part");
    } else {
        *parts = flat;
        *shares = normalized(&flat_shares);
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
            Layout::Split { axis, parts, .. } => {
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

    /// Each part's share of the split at `path` in the only tab, in whole
    /// percent.
    fn percents(state: &WorkspaceState, path: &[u16]) -> Vec<u64> {
        let Some(Layout::Split { shares, .. }) = only_tab(state).layout.at(path) else {
            panic!("no split at {path:?}");
        };
        let total: u64 = shares.iter().map(|&share| u64::from(share)).sum();
        shares
            .iter()
            .map(|&share| u64::from(share) * 100 / total)
            .collect()
    }

    #[test]
    fn a_pane_down_a_tabs_edge_takes_its_share_of_the_whole_tab() {
        let mut state = WorkspaceState::new();
        state.new_workspace("work".into(), TerminalId(1));
        let tab = only_tab(&state).id;
        state.split_tab(tab, Side::Left, TerminalId(2), Some(250));
        assert_eq!(shape(&only_tab(&state).layout), "(2 | 1)");
        assert_eq!(percents(&state, &[]), [25, 75]);
        // Joining a split along the same axis, the others keep their part
        // of what's left.
        state.split_tab(tab, Side::Right, TerminalId(3), Some(500));
        assert_eq!(percents(&state, &[]), [12, 37, 50]);
        // Across it, it goes round the whole tab.
        state.split_tab(tab, Side::Below, TerminalId(4), None);
        assert_eq!(shape(&only_tab(&state).layout), "((2 | 1 | 3) / 4)");
        assert_eq!(percents(&state, &[]), [50, 50]);
        assert_eq!(only_tab(&state).focused.0, 4);
    }

    #[test]
    fn shares_follow_resizing_splitting_and_closing() {
        let mut state = WorkspaceState::new();
        let one = state.new_workspace("work".into(), TerminalId(1));
        let tab = only_tab(&state).id;
        let two = state.split(one, Side::Right, TerminalId(2));
        assert_eq!(percents(&state, &[]), [50, 50]);
        state.resize_split(tab, &[], &[3, 1]).unwrap();
        assert_eq!(percents(&state, &[]), [75, 25]);
        // A pane joining the split gets the average share.
        state.split(two, Side::Right, TerminalId(3));
        assert_eq!(percents(&state, &[]), [50, 16, 33]);
        state.remove_terminal(TerminalId(3));
        assert_eq!(percents(&state, &[]), [75, 25]);

        // Asking for a split that isn't there, or a share of nothing.
        assert!(matches!(
            state.resize_split(tab, &[1], &[1, 1]),
            Err(Error::NoSuchSplit(_))
        ));
        assert!(matches!(
            state.resize_split(tab, &[], &[1, 1, 1]),
            Err(Error::NoSuchSplit(_))
        ));
        assert!(matches!(
            state.resize_split(tab, &[], &[1, 0]),
            Err(Error::EmptyShare)
        ));
        assert!(matches!(
            state.resize_split(TabId(99), &[], &[1, 1]),
            Err(Error::NoSuchTab(_))
        ));
    }

    #[test]
    fn a_split_merged_into_another_shares_out_its_part() {
        let mut state = WorkspaceState::new();
        let one = state.new_workspace("work".into(), TerminalId(1));
        let two = state.split(one, Side::Below, TerminalId(2));
        state.split(two, Side::Right, TerminalId(3));
        state.split(two, Side::Below, TerminalId(4));
        let tab = only_tab(&state).id;
        state.resize_split(tab, &[], &[1, 3]).unwrap();
        state.resize_split(tab, &[1, 0], &[2, 1]).unwrap();
        assert_eq!(shape(&only_tab(&state).layout), "(1 / ((2 / 4) | 3))");
        state.remove_terminal(TerminalId(3));
        assert_eq!(shape(&only_tab(&state).layout), "(1 / 2 / 4)");
        assert_eq!(percents(&state, &[]), [25, 50, 25]);
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
        let panes = state.tab_panes(tab).unwrap();
        assert_eq!(panes[0].content, Content::Terminal(TerminalId(1)));
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

    #[test]
    fn splits_go_no_deeper_than_a_window_reads() {
        // Splitting the newest pane across the last split, each time.
        let mut state = WorkspaceState::new();
        let mut pane = state.new_workspace("a".into(), TerminalId(1));
        for n in 0.. {
            let side = if n % 2 == 0 { Side::Right } else { Side::Below };
            if state.check_split(pane, side).is_err() {
                assert_eq!(n, MAX_NESTING);
                break;
            }
            pane = state.split(pane, side, TerminalId(n + 2));
        }
        assert_eq!(only_tab(&state).layout.depth(), MAX_NESTING);
        // Along the same axis it joins the split, which is still fine.
        let last = if MAX_NESTING.is_multiple_of(2) {
            Side::Below
        } else {
            Side::Right
        };
        assert!(state.check_split(pane, last).is_ok());

        // And splitting the whole tab, each time across.
        let mut state = WorkspaceState::new();
        state.new_workspace("b".into(), TerminalId(1));
        let tab = only_tab(&state).id;
        for n in 0.. {
            let side = if n % 2 == 0 { Side::Above } else { Side::Left };
            if let Err(err) = state.check_split_tab(tab, side) {
                assert_eq!(n, MAX_NESTING);
                assert!(err.to_string().contains("as deep as they can go"), "{err}");
                break;
            }
            state.split_tab(tab, side, TerminalId(n + 2), None);
        }
        assert_eq!(only_tab(&state).layout.depth(), MAX_NESTING);
    }

    #[test]
    fn a_layout_handed_over_too_deep_is_flattened() {
        // Made as a build without the limit could.
        let mut state = WorkspaceState::new();
        let mut pane = state.new_workspace("a".into(), TerminalId(1));
        for n in 0..MAX_NESTING + 40 {
            let side = if n % 2 == 0 { Side::Right } else { Side::Below };
            pane = state.split(pane, side, TerminalId(n + 2));
        }
        let before = only_tab(&state).layout.panes();
        let resumed = WorkspaceState::resume(&state.hand_over()).unwrap();
        let tab = only_tab(&resumed);
        assert_eq!(tab.layout.depth(), MAX_NESTING);
        assert_eq!(tab.layout.panes(), before, "every pane kept, in order");
        assert_eq!(tab.focused, pane);
    }

    #[test]
    fn the_home_is_told_unless_its_the_root_used_for_no_home() {
        let home = |path: &str| {
            let state = WorkspaceState::with_home(std::path::Path::new(path));
            state.get().home.clone()
        };
        assert_eq!(home("/home/j").as_deref(), Some("/home/j"));
        assert_eq!(home("/"), None);
    }
}
