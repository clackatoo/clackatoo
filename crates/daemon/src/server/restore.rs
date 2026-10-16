//! Workspaces kept from one run of the daemon to the next: written as
//! they change, and made again as a daemon starts afresh.
//!
//! What's kept, and the file it's kept in, are in [`crate::saved`]. Here,
//! the daemon notes that something kept has changed and writes the file a
//! moment later ([`KEEP_AFTER`]), on the writer's thread, and once more
//! as it stops or upgrades. What changes are the workspaces themselves
//! ([`Daemon::workspaces_changed`]), a shell's directory or whether it's
//! running a command (as the shell marks them, see [`Daemon::note_shell`]),
//! an explorer's root, a buffer's file, a chat's session, and where
//! windows say editors are ([`ClientMessage::Place`]).
//!
//! A daemon starting afresh, not by an upgrade, which carries everything
//! over itself, makes the workspaces again ([`Daemon::restore_workspaces`]):
//! each terminal as a new shell in the directory it was in, or the home
//! if that's gone; each editor on its file, which windows put back where
//! it was; each explorer and folder at its root; boards and reviews; and
//! each chat on its agent's session, which it opens only once it's shown
//! or sent a message, loading it so the conversation so far is told, and
//! nothing told acts (see `chat::resume`). A terminal whose shell was
//! running a command comes back with it in [`Workspaces::restored`], for
//! windows to offer to put at the new shell's prompt; it's never run by
//! itself, nor put there unasked. Nothing in the
//! file can stop the daemon starting: a file that can't be read is said in
//! the log and set aside, and the daemon starts with no workspaces.
//!
//! [`ClientMessage::Place`]: clackatoo_protocol::ClientMessage::Place
//! [`Workspaces::restored`]: clackatoo_protocol::Workspaces::restored

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    AgentTools, Axis, BoardId, BufferId, Content, Layout, NewReview, NewTerminal, OpenFile,
    OpenFiles, PaneId, PaneTerminal, Place, Restored, ResumeSession, ReviewScope, Size, TerminalId,
};

use clackatoo_term::Progress;

use super::{Daemon, Hosted};
use crate::chat::HostedChat;
use crate::saved::{self, Pane, Scope, State, Tools};

/// How long after something kept changes the workspaces are written: so
/// a burst of changes, such as a layout being made, is written once, and
/// they're written at most this often.
pub(super) const KEEP_AFTER: Duration = Duration::from_secs(1);

/// How long the daemon waits, as it stops or upgrades, for the workspaces
/// to be written.
const KEEP_WAIT: Duration = Duration::from_secs(1);

/// The most panes made again as the daemon starts, so a file that holds
/// thousands can't start thousands of shells.
const MOST_PANES: usize = 512;

/// The longest command line kept to offer to run again, in bytes.
const LONGEST_COMMAND: usize = 1024;

/// The size terminals and chats are made again at, until a window
/// showing them says its own.
fn size() -> Size {
    Size::new(80, 24).expect("a size a terminal can be")
}

/// Keeping the workspaces: where, and what's waiting to be written.
#[derive(Debug, Default)]
pub(super) struct Keeping {
    /// The file they're kept in, while this daemon holds its lock.
    path: Option<PathBuf>,
    /// The lock, held while it's open.
    lock: Option<File>,
    /// When they're next written, as something kept has changed since
    /// they last were.
    due: Option<Instant>,
    /// What was last written, so the same isn't written again: at first,
    /// no workspaces.
    last: Option<State>,
    /// The thread writing them, started with the first write.
    writer: Option<saved::Writer>,
    /// For tests: a file the thread waits for before each write (see
    /// [`DaemonConfig::keep_gate`](crate::DaemonConfig::keep_gate)).
    gate: Option<PathBuf>,
    /// Where windows last said editor panes are, with the buffer each
    /// showed then.
    places: HashMap<PaneId, (BufferId, Place)>,
    /// What each terminal's shell last said.
    shells: HashMap<TerminalId, Shell>,
}

/// What a shell last said: its directory, and its latest block's number
/// and whether a command had started in it.
type Shell = (Option<String>, Option<(u32, bool)>);

impl Keeping {
    /// Keeping in the file `config` says, if this daemon is the only one
    /// doing so (see [`saved::lock`]), or else nothing. Starting `afresh`,
    /// it waits a moment for the lock, if it's held; carrying on from an
    /// upgrade, the build before has let it go.
    pub(super) fn new(config: &crate::DaemonConfig, afresh: bool) -> Self {
        let Some(path) = config.workspaces.clone() else {
            return Self::default();
        };
        let patience = if afresh {
            saved::LOCK_PATIENCE
        } else {
            Duration::ZERO
        };
        if let Some(dir) = path.parent() {
            // clackatoo's own folder is the user's alone; one they named,
            // as their home, is theirs to say.
            if crate::state_dir().is_some_and(|state| state == dir) {
                if let Err(err) = saved::tighten(dir) {
                    log::warn!("{} couldn't be made yours alone: {err}", dir.display());
                }
            } else if saved::open_to_others(dir) {
                log::warn!(
                    "others can get into {}, where the workspaces are kept",
                    dir.display()
                );
            }
        }
        match saved::lock(&path, patience) {
            Ok(lock) => Self {
                path: Some(path),
                lock: Some(lock),
                gate: config.keep_gate.clone(),
                // Nothing is written until there's something: a daemon
                // started `--fresh`, or after an upgrade, leaves the file
                // as it is until something changes.
                last: Some(State {
                    clackatoo_workspaces: saved::VERSION,
                    ..State::default()
                }),
                ..Self::default()
            },
            Err(why) => {
                log::warn!("{why}");
                Self::default()
            }
        }
    }

    /// When the workspaces are next to be written.
    pub(super) fn due(&self) -> Option<Instant> {
        self.due
    }
}

impl Daemon {
    /// Notes that something kept has changed: the workspaces are written
    /// a moment later.
    pub(super) fn keep_soon(&mut self) {
        if self.keeping.path.is_some() && self.keeping.due.is_none() {
            self.keeping.due = Some(Instant::now() + KEEP_AFTER);
        }
    }

    /// Writes the workspaces, if it's time to.
    pub(super) fn keep_if_due(&mut self) {
        if self.keeping.due.is_some_and(|due| due <= Instant::now()) {
            self.keeping.due = None;
            self.keep();
        }
    }

    /// Has the writer's thread write the workspaces as they are now, if
    /// they've changed since they were last written.
    fn keep(&mut self) {
        let Some(path) = self.keeping.path.clone() else {
            return;
        };
        let state = self.kept_state();
        if self.keeping.last.as_ref() == Some(&state) {
            return;
        }
        if self.keeping.writer.is_none() {
            match saved::Writer::start(path, self.keeping.gate.clone()) {
                Ok(writer) => self.keeping.writer = Some(writer),
                Err(err) => {
                    log::error!("the workspaces can't be kept: no thread to write them: {err}");
                    return;
                }
            }
        }
        if let Some(writer) = &self.keeping.writer {
            writer.write(state.clone());
            self.keeping.last = Some(state);
        }
    }

    /// Writes the workspaces as they are now, as the daemon stops or
    /// upgrades, waiting a moment for them to be written. A daemon that
    /// stops drops its terminals only after this, so what's written is
    /// what there was, not the panes closing.
    pub(super) fn keep_last(&mut self) {
        self.keeping.due = None;
        self.keep();
        if let Some(writer) = self.keeping.writer.take()
            && !writer.stop(KEEP_WAIT)
        {
            log::warn!(
                "the workspaces weren't written within {} s",
                KEEP_WAIT.as_secs()
            );
        }
        // Another build may keep them from here on: an upgrade's.
        self.keeping.lock = None;
        self.keeping.path = None;
    }

    /// Takes in where a window says editor `pane` is, to keep. Anything
    /// else, or an editor gone, is passed over.
    pub(super) fn serve_place(&mut self, pane: PaneId, place: Place) {
        let Ok(Content::Buffer(buffer)) = self.workspaces.check_pane(pane) else {
            return;
        };
        if self.keeping.places.insert(pane, (buffer, place)) != Some((buffer, place)) {
            self.keep_soon();
        }
        // A window has the editor where it wants it now.
        if self.workspaces.forget_place(pane) {
            self.workspaces_changed();
        }
    }

    /// `bytes` were typed into terminal `id`, by any client or agent: an
    /// offer to run again the command it was running is gone, so it's
    /// never put after what's half typed. A window saying only that it
    /// gained or lost the focus, as a program asked (mode 1004), types
    /// nothing.
    pub(super) fn typed_into(&mut self, id: TerminalId, bytes: &[u8]) {
        if only_focus_reports(bytes) {
            return;
        }
        if let Some(pane) = self.workspaces.get().pane_showing(id)
            && self.workspaces.forget_ran(pane)
        {
            self.workspaces_changed();
        }
    }

    /// Dismisses the offer to run again the command `pane`'s terminal was
    /// running when the daemon last stopped.
    pub(super) fn serve_dismiss(&mut self, pane: PaneId) {
        if self.workspaces.forget_ran(pane) {
            self.workspaces_changed();
        }
    }

    /// Notes what terminal `id`'s shell says now, as it's read: a new
    /// directory, or a command starting or ending, is kept. A command
    /// starting, even one that has ended by now, ends the offer to run
    /// again the one it was running when the daemon last stopped. Costs a
    /// comparison when nothing changed.
    pub(super) fn note_shell(&mut self, id: TerminalId) {
        // A daemon that keeps nothing made nothing again to offer.
        if self.keeping.path.is_none() {
            return;
        }
        let Some(Hosted::Pty(terminal)) = self.terminals.get(&id).map(|slot| &slot.terminal) else {
            return;
        };
        let emulator = terminal.emulator();
        let cwd = emulator.cwd();
        let block = (emulator.latest_block())
            .map(|(block, progress)| (block, progress != Progress::Prompt));
        let known = self.keeping.shells.get(&id);
        if known.is_some_and(|(known, was)| known.as_deref() == cwd && *was == block) {
            return;
        }
        let started =
            block.is_some_and(|(_, started)| started) && known.is_none_or(|(_, was)| *was != block);
        self.keeping
            .shells
            .insert(id, (cwd.map(str::to_owned), block));
        self.keep_soon();
        if started
            && let Some(pane) = self.workspaces.get().pane_showing(id)
            && self.workspaces.forget_ran(pane)
        {
            self.workspaces_changed();
        }
    }

    /// The workspaces, as they're kept.
    fn kept_state(&mut self) -> State {
        // Forgotten for what's gone, as the file is written.
        let terminals = &self.terminals;
        self.keeping
            .shells
            .retain(|id, _| terminals.contains_key(id));
        let workspaces = self.workspaces.get();
        let shown: Vec<(PaneId, Content)> = (workspaces.list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .map(|pane| (pane.id, pane.content))
            .collect();
        (self.keeping.places)
            .retain(|pane, (buffer, _)| shown.contains(&(*pane, Content::Buffer(*buffer))));
        let mut state = State {
            clackatoo_workspaces: saved::VERSION,
            workspaces: Vec::new(),
            active: None,
        };
        for workspace in &workspaces.list {
            let mut tabs = Vec::new();
            let mut active_tab = 0;
            for tab in &workspace.tabs {
                let mut kept = Vec::new();
                let Some(layout) = self.kept_layout(&tab.layout, &mut kept) else {
                    continue;
                };
                if tab.id == workspace.active_tab {
                    active_tab = tabs.len();
                }
                tabs.push(saved::Tab {
                    name: tab.name.clone(),
                    layout,
                    focused: (kept.iter().position(|&pane| pane == tab.focused)).unwrap_or(0),
                });
            }
            if tabs.is_empty() {
                continue;
            }
            if workspaces.active == Some(workspace.id) {
                state.active = Some(state.workspaces.len());
            }
            state.workspaces.push(saved::Workspace {
                name: workspace.name.clone(),
                tabs,
                active_tab,
            });
        }
        state
    }

    /// `layout`, as it's kept, with the IDs of the panes kept added to
    /// `kept` in order: `None` if none is.
    fn kept_layout(&self, layout: &Layout, kept: &mut Vec<PaneId>) -> Option<saved::Layout> {
        match layout {
            Layout::Pane(pane) => {
                let saved = self.kept_pane(pane.id, pane.content)?;
                kept.push(pane.id);
                Some(saved::Layout::Pane(saved))
            }
            Layout::Split {
                axis,
                parts,
                shares,
            } => {
                let (mut parts, mut shares): (Vec<saved::Layout>, Vec<u32>) =
                    (parts.iter().zip(shares))
                        .filter_map(|(part, &share)| Some((self.kept_layout(part, kept)?, share)))
                        .unzip();
                match parts.len() {
                    0 => None,
                    1 => parts.pop(),
                    _ => Some(saved::Layout::Split {
                        left_to_right: *axis == Axis::LeftToRight,
                        parts: std::mem::take(&mut parts),
                        shares: std::mem::take(&mut shares),
                    }),
                }
            }
        }
    }

    /// Pane `id`, showing `content`, as it's kept: `None` for one that
    /// isn't, a buffer with no file yet or a chat's browser page.
    fn kept_pane(&self, id: PaneId, content: Content) -> Option<Pane> {
        let restored = self.workspaces.restored(id);
        match content {
            Content::Terminal(terminal) => match &self.terminals.get(&terminal)?.terminal {
                Hosted::Pty(terminal) => {
                    let emulator = terminal.emulator();
                    Some(Pane::Terminal {
                        cwd: (terminal.own_directory())
                            .or_else(|| emulator.cwd().map(str::to_owned)),
                        // An offer still open stays open after a restart.
                        // Otherwise, what the shell said it runs, if what's
                        // in front of it is that, never what a program
                        // printed.
                        ran: (restored.and_then(|restored| restored.ran.clone())).or_else(|| {
                            emulator
                                .running_command()
                                .and_then(kept_command)
                                .filter(|command| terminal.runs_in_front(command))
                        }),
                    })
                }
                Hosted::Chat(chat) => Some(Pane::Chat {
                    id: terminal.0,
                    agent: chat.agent().to_owned(),
                    session: chat.session().map(str::to_owned),
                    title: chat.info().title,
                    cwd: chat.working_directory().to_owned(),
                    model: chat.model().map(str::to_owned),
                    tools: match chat.tools() {
                        AgentTools::Off => Tools::Off,
                        AgentTools::Ask => Tools::Ask,
                        AgentTools::Allow => Tools::Allow,
                    },
                    task: chat.task(),
                    browser: chat.browser(),
                }),
            },
            Content::Buffer(buffer) => {
                let slot = self.buffers.get(&buffer)?;
                // Changes not saved are in its recovery file, by which it
                // comes back with them, as a buffer with no file yet can
                // only.
                let recovery = (slot.buffer.modified() && self.config.recovery.is_some())
                    .then(|| self.recovery_key(buffer));
                let path = if slot.buffer.is_blank() {
                    recovery.as_ref()?;
                    String::new()
                } else {
                    slot.buffer.path().to_str()?.to_owned()
                };
                Some(Pane::Editor {
                    path,
                    recovery,
                    place: (self.keeping.places.get(&id).map(|&(_, place)| place))
                        .or_else(|| restored.and_then(|restored| restored.place))
                        .map(|place| saved::Place {
                            line: place.line,
                            column: place.column,
                            top: place.top,
                        }),
                })
            }
            Content::Folder(files) => Some(Pane::Folder {
                root: self.files_root(files)?.to_str()?.to_owned(),
            }),
            Content::Files(files) => Some(Pane::Explorer {
                root: self.files_root(files)?.to_str()?.to_owned(),
            }),
            Content::Board(board) => Some(Pane::Board { id: board.0 }),
            Content::Review(review) => {
                let (chat, scope, root) = self.review_of(review)?;
                Some(Pane::Review {
                    chat: chat.map(|chat| chat.0),
                    scope: match scope {
                        ReviewScope::Turn(turn) => Scope::Turn(turn),
                        ReviewScope::Chat => Scope::Chat,
                        ReviewScope::Base => Scope::Base,
                    },
                    root: root.to_str()?.to_owned(),
                })
            }
            Content::Browser(_) | Content::Other { .. } => None,
        }
    }

    /// Makes the workspaces kept from the last run again, as a daemon
    /// starting afresh does, unless it doesn't keep them. A file that
    /// can't be read, or is from a newer build, is set aside, and the
    /// daemon starts with none. Nothing here fails the start.
    pub fn restore_workspaces(&mut self) {
        let Some(path) = self.keeping.path.clone() else {
            return;
        };
        let state = match saved::read(&path) {
            Ok(Some(state)) => {
                // Kept beside, as panes that can't be made now, as on a
                // disk not mounted, would be left out of the next write.
                if let Err(err) = saved::keep_previous(&path) {
                    log::warn!("the workspaces kept couldn't be copied aside: {err}");
                }
                state
            }
            Ok(None) => return,
            Err(err) => {
                log::error!(
                    "the workspaces kept in {} couldn't be read, so none are made again: {err}",
                    path.display()
                );
                match saved::set_aside(&path) {
                    Ok(aside) => log::info!("that file is kept as {}", aside.display()),
                    Err(err) => log::error!("that file couldn't be set aside: {err}"),
                }
                return;
            }
        };
        let started = Instant::now();
        let mut room = MOST_PANES;
        // Chats first, so reviews of them find them made.
        let mut chats = HashMap::new();
        for workspace in &state.workspaces {
            for tab in &workspace.tabs {
                self.restore_chats(&tab.layout, &mut chats, &mut room);
            }
        }
        let mut restored = Vec::new();
        let mut made = Vec::new();
        for workspace in &state.workspaces {
            let mut tabs = Vec::new();
            let mut active_tab = 0;
            for (index, tab) in workspace.tabs.iter().enumerate() {
                let mut panes = Vec::new();
                let Some(layout) = self.restore_layout(
                    &tab.layout,
                    &mut chats,
                    &mut room,
                    &mut panes,
                    &mut restored,
                ) else {
                    continue;
                };
                if index == workspace.active_tab {
                    active_tab = tabs.len();
                }
                let focused = panes.get(tab.focused).copied().flatten();
                tabs.push((tab.name.clone(), layout, focused));
            }
            made.push(if tabs.is_empty() {
                None
            } else {
                Some(
                    self.workspaces
                        .add_restored(&workspace.name, tabs, active_tab),
                )
            });
        }
        let active = (state.active)
            .and_then(|active| made.get(active).copied().flatten())
            .or_else(|| made.iter().copied().flatten().next());
        if let Some(active) = active {
            self.workspaces.set_active(active);
        }
        self.workspaces.set_restored(restored);
        let panes = MOST_PANES - room;
        if room == 0 {
            log::warn!("no more than the first {MOST_PANES} panes kept were made again");
        }
        log::info!(
            "{} workspaces and {panes} panes made again in {} ms",
            made.iter().flatten().count(),
            started.elapsed().as_millis()
        );
        self.workspaces_changed();
    }

    /// Takes where editors were from the workspaces kept, as a daemon
    /// carries on from an upgrade, which hands over no places: each editor
    /// of a file kept, where an editor of that file was. Windows say where
    /// the editors they show are as they connect again, but not those of
    /// workspaces they don't show, whose places would otherwise be lost
    /// with the next write.
    pub(super) fn keep_places_kept(&mut self) {
        let Some(path) = &self.keeping.path else {
            return;
        };
        let Ok(Some(state)) = saved::read(path) else {
            return;
        };
        let mut kept = HashMap::new();
        for tab in state
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
        {
            editor_places(&tab.layout, &mut kept);
        }
        let panes: Vec<(PaneId, BufferId)> = (self.workspaces.get().list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .filter_map(|pane| Some((pane.id, pane.content.buffer()?)))
            .collect();
        for (pane, buffer) in panes {
            let path = (self.buffers.get(&buffer)).and_then(|slot| slot.buffer.path().to_str());
            if let Some(place) = path.and_then(|path| kept.get(path)) {
                let place = Place {
                    line: place.line,
                    column: place.column,
                    top: place.top,
                };
                self.keeping.places.insert(pane, (buffer, place));
            }
        }
    }

    /// Makes the chats in `layout` again, each by its ID in the run that
    /// kept them, in `chats`, as yet in no pane.
    fn restore_chats(
        &mut self,
        layout: &saved::Layout,
        chats: &mut HashMap<u32, (TerminalId, bool)>,
        room: &mut usize,
    ) {
        match layout {
            saved::Layout::Split { parts, .. } => {
                for part in parts {
                    self.restore_chats(part, chats, room);
                }
            }
            saved::Layout::Pane(Pane::Chat {
                id,
                agent,
                session,
                title,
                cwd,
                model,
                tools,
                task,
                browser,
            }) => {
                if *room == 0 || chats.contains_key(id) {
                    return;
                }
                *room -= 1;
                let chat = self.next_terminal();
                let spec = self.agent_spec(agent);
                let tools = match tools {
                    Tools::Off => AgentTools::Off,
                    Tools::Ask => AgentTools::Ask,
                    Tools::Allow => AgentTools::Allow,
                };
                let mut hosted =
                    HostedChat::new(chat, size(), cwd.clone(), &spec, tools, *task, *browser);
                if let Some(model) = model {
                    hosted.want_model(model.clone());
                }
                if let Some(session) = session {
                    hosted.go_on_with(ResumeSession {
                        session: session.clone(),
                        title: title.clone(),
                    });
                }
                hosted.wait_to_be_shown();
                self.add_chat(hosted);
                log::info!("chat {chat} made again, with {agent}");
                chats.insert(*id, (chat, false));
            }
            saved::Layout::Pane(_) => {}
        }
    }

    /// Makes `layout` again: `None` if none of its panes could be. Each
    /// pane's ID is added to `panes` in order, or `None` for one that
    /// couldn't be made, and what each has to show for the restart to
    /// `restored`.
    fn restore_layout(
        &mut self,
        layout: &saved::Layout,
        chats: &mut HashMap<u32, (TerminalId, bool)>,
        room: &mut usize,
        panes: &mut Vec<Option<PaneId>>,
        restored: &mut Vec<Restored>,
    ) -> Option<Layout> {
        match layout {
            saved::Layout::Pane(pane) => {
                let content = self.restore_pane(pane, chats, room);
                let made = content.map(|content| self.workspaces.fresh_pane(content));
                panes.push(made.map(|made| made.id));
                let made = made?;
                let (ran, place) = match pane {
                    Pane::Terminal { ran, .. } => (ran.as_deref().and_then(kept_command), None),
                    Pane::Editor { place, .. } => (
                        None,
                        place.and_then(|place| self.within(made.content, place)),
                    ),
                    _ => (None, None),
                };
                if ran.is_some() || place.is_some() {
                    restored.push(Restored {
                        pane: made.id,
                        ran,
                        place,
                    });
                }
                Some(Layout::Pane(made))
            }
            saved::Layout::Split {
                left_to_right,
                parts,
                shares,
            } => {
                let shares_fit = shares.len() == parts.len() && !shares.contains(&0);
                let mut made = Vec::new();
                let mut kept_shares = Vec::new();
                for (index, part) in parts.iter().enumerate() {
                    if let Some(part) = self.restore_layout(part, chats, room, panes, restored) {
                        made.push(part);
                        kept_shares.push(if shares_fit { shares[index] } else { 1 });
                    }
                }
                match made.len() {
                    0 => None,
                    1 => made.pop(),
                    _ => Some(Layout::Split {
                        axis: if *left_to_right {
                            Axis::LeftToRight
                        } else {
                            Axis::TopToBottom
                        },
                        parts: made,
                        shares: kept_shares,
                    }),
                }
            }
        }
    }

    /// `place`, as kept, within the text of the buffer `content` shows as
    /// it's read now, which may have changed: its lines no further than
    /// the text's last, and the column no further than its line's end.
    fn within(&self, content: Content, place: saved::Place) -> Option<Place> {
        let Content::Buffer(buffer) = content else {
            return None;
        };
        let text = self.buffers.get(&buffer)?.buffer.text();
        let last = text.len_lines().saturating_sub(1);
        let clamp = |line: u64| usize::try_from(line).unwrap_or(usize::MAX).min(last);
        let line = clamp(place.line);
        let length = text.line(line).len_bytes();
        let length = if text.line(line).bytes().last() == Some(b'\n') {
            length - 1
        } else {
            length
        };
        Some(Place {
            line: line as u64,
            column: place.column.min(length as u64),
            top: clamp(place.top) as u64,
        })
    }

    /// Makes what `pane` showed again: `None` if it can't be, as for a
    /// file that's gone, saying why in the log.
    fn restore_pane(
        &mut self,
        pane: &Pane,
        chats: &mut HashMap<u32, (TerminalId, bool)>,
        room: &mut usize,
    ) -> Option<Content> {
        if let Pane::Chat { id, .. } = pane {
            // In one pane at most, as a file mended by hand could have
            // it in two.
            return match chats.get_mut(id) {
                Some((chat, shown)) if !*shown => {
                    *shown = true;
                    Some(Content::Terminal(*chat))
                }
                _ => None,
            };
        }
        if *room == 0 {
            return None;
        }
        *room -= 1;
        let made = match pane {
            Pane::Terminal { cwd, .. } => {
                // A stat, as opening a file is.
                let cwd = (cwd.as_deref())
                    .filter(|cwd| Path::new(cwd).is_dir())
                    .map_or_else(|| self.config.home.display().to_string(), str::to_owned);
                let terminal = NewTerminal {
                    size: size(),
                    command: None,
                    cwd: Some(cwd),
                    record_to: None,
                };
                self.pane_content(PaneTerminal::New(terminal), None)
            }
            Pane::Editor { path, recovery, .. } => {
                // Its changes not saved came back as a buffer of their own,
                // with no file yet, or with its file as it was.
                let recovered = (recovery.as_deref())
                    .and_then(|key| self.recovered_from(key))
                    .filter(|buffer| self.buffers.contains_key(buffer));
                if let Some(buffer) = recovered {
                    return Some(Content::Buffer(buffer));
                }
                // A file that's gone isn't made again empty.
                if path.is_empty() || !Path::new(path).is_file() {
                    log::info!("{path} isn't there: its editor isn't made again");
                    return None;
                }
                let file = OpenFile {
                    path: path.clone(),
                    cwd: None,
                };
                self.pane_content(PaneTerminal::File(file), None)
            }
            Pane::Folder { root } => {
                let file = OpenFile {
                    path: format!("{}/", root.trim_end_matches('/')),
                    cwd: None,
                };
                self.pane_content(PaneTerminal::File(file), None)
            }
            Pane::Explorer { root } => {
                let open = OpenFiles {
                    dir: Some(root.clone()),
                };
                self.pane_content(PaneTerminal::Files(open), None)
            }
            // Boards are read on a thread: a pane showing one that isn't
            // there once they have been is closed then.
            Pane::Board { id } => Ok(Content::Board(BoardId(*id))),
            Pane::Review { chat, scope, root } => {
                let new = NewReview {
                    chat: chat.and_then(|chat| chats.get(&chat).map(|&(chat, _)| chat)),
                    scope: match *scope {
                        Scope::Turn(turn) => ReviewScope::Turn(turn),
                        Scope::Chat => ReviewScope::Chat,
                        Scope::Base => ReviewScope::Base,
                    },
                    cwd: Some(root.clone()),
                };
                self.pane_content(PaneTerminal::Review(new), None)
            }
            Pane::Chat { .. } => unreachable!("chats are made first"),
        };
        made.map_err(|err| log::warn!("a pane kept couldn't be made again: {err}"))
            .ok()
    }

    /// Closes the panes showing boards there aren't, once the boards have
    /// been read: those made again from the last run of boards since
    /// deleted.
    pub(super) fn close_missing_boards(&mut self) {
        if self.boards.ready().is_err() {
            return;
        }
        let missing: Vec<PaneId> = (self.workspaces.get().list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .filter(|pane| {
                matches!(pane.content, Content::Board(board) if self.boards.list.get(board).is_none())
            })
            .map(|pane| pane.id)
            .collect();
        for pane in missing {
            log::info!("pane {pane}: its board isn't there");
            self.close_board_pane(pane);
        }
    }

    /// A chat being shown: one made again as the daemon started asks for
    /// its session now (see [`HostedChat::opens_when_shown`]).
    pub(super) fn show_restored_chat(&mut self, id: TerminalId) {
        if self.chat(id).is_some_and(HostedChat::opens_when_shown) {
            log::info!("chat {id}: shown, so it goes on with its session");
            self.connect(id);
            self.collect_damage(id);
        }
    }
}

/// Where each file's editor in `layout` was, as kept, added to `into`:
/// the first of several editors of one file.
fn editor_places(layout: &saved::Layout, into: &mut HashMap<String, saved::Place>) {
    match layout {
        saved::Layout::Split { parts, .. } => {
            for part in parts {
                editor_places(part, into);
            }
        }
        saved::Layout::Pane(Pane::Editor {
            path,
            place: Some(place),
            ..
        }) => {
            into.entry(path.clone()).or_insert(*place);
        }
        saved::Layout::Pane(_) => {}
    }
}

/// Whether `bytes` are nothing but reports of a window gaining or losing
/// the focus, `ESC [ I` and `ESC [ O`, which a program asks for.
fn only_focus_reports(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len().is_multiple_of(3)
        && (bytes.chunks(3)).all(|report| report == b"\x1b[I" || report == b"\x1b[O")
}

/// `command`, a command line a shell said it was running, if it's one to
/// keep, to offer to run again after a restart: not one starting with a
/// space, which shells leave out of their history, as the window's command
/// line does (bash's `ignorespace`, zsh's `HIST_IGNORE_SPACE`, fish's
/// private lines), nor one that would keep a secret, as `clackatoo devices
/// allow CODE` would; and only one line, with no control characters, marks
/// that reorder text or runs of spaces, as it's shown, and put at a new
/// shell's prompt.
pub(super) fn kept_command(command: &str) -> Option<String> {
    // A run of spaces could push what follows out of sight.
    let spread = {
        let trimmed = command.trim_end();
        (trimmed.chars().zip(trimmed.chars().skip(1)))
            .any(|(one, next)| one.is_whitespace() && next.is_whitespace())
    };
    let secret = {
        let words: Vec<&str> = command.split_whitespace().take(3).collect();
        words.len() == 3 && words[0].ends_with("clackatoo") && words[1..] == ["devices", "allow"]
    };
    let shown = |ch: char| !ch.is_control() && !super::tools::invisible(ch);
    let kept = !command.trim().is_empty()
        && command.len() <= LONGEST_COMMAND
        && !command.starts_with(char::is_whitespace)
        && command.chars().all(shown)
        && !spread
        && !secret;
    kept.then(|| command.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::kept_command;

    #[test]
    fn focus_reports_alone_type_nothing() {
        assert!(super::only_focus_reports(b"\x1b[I"));
        assert!(super::only_focus_reports(b"\x1b[O\x1b[I"));
        assert!(!super::only_focus_reports(b""));
        assert!(!super::only_focus_reports(b"\x1b[Ix"));
        assert!(!super::only_focus_reports(b"\x1b[A"));
        assert!(!super::only_focus_reports(b"ls"));
    }

    #[test]
    fn commands_kept_are_one_line_and_never_ones_kept_out_of_history() {
        assert_eq!(
            kept_command("cargo watch -x test").as_deref(),
            Some("cargo watch -x test")
        );
        assert_eq!(kept_command("htop ").as_deref(), Some("htop"));
        assert_eq!(kept_command(" export TOKEN=hunter2"), None);
        assert_eq!(kept_command("\tcurl -H secret"), None);
        assert_eq!(kept_command(""), None);
        assert_eq!(kept_command("   "), None);
        assert_eq!(kept_command("for x in a\n  echo $x\nend"), None);
        assert_eq!(kept_command("echo \x1b[31m"), None);
        assert_eq!(kept_command("echo \u{202e}txt.exe"), None);
        assert_eq!(kept_command("clackatoo devices allow 123-456"), None);
        assert_eq!(
            kept_command("/usr/bin/clackatoo devices allow 123-456"),
            None
        );
        assert!(kept_command("clackatoo devices").is_some());
        assert_eq!(kept_command(&"x".repeat(2000)), None);
        let pushed = format!("ls{}; rm -rf ~", " ".repeat(200));
        assert_eq!(kept_command(&pushed), None);
        assert_eq!(kept_command("ls \u{3000}x"), None);
    }
}
