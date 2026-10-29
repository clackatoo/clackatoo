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
//! nothing told acts (see `chat::resume`); a chat an agent started is
//! its parent's subagent again, as its profile, with no more than it had
//! ([`Daemon::restore_standing`]). A terminal whose shell was
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    AgentTools, Axis, BoardId, BufferId, Content, Layout, NewReview, NewTerminal, OpenFile,
    OpenFiles, PaneId, PaneTerminal, Place, Restored, ResumeSession, ReviewScope, Size, TerminalId,
    TerminalStatus,
};

use clackatoo_term::Progress;

use super::{Daemon, Hosted};
use crate::chat::{HostedChat, Standing};
use crate::profiles::Limits;
use crate::saved::{self, Pane, Scope, State, Tools};
use crate::workspaces::Borrowed;

/// How long after something kept changes the workspaces are written: so
/// a burst of changes, such as a layout being made, is written once, and
/// they're written at most this often.
pub(super) const KEEP_AFTER: Duration = Duration::from_secs(1);

/// How long after a write fails the workspaces are written again, changed
/// or not.
const KEEP_AGAIN: Duration = Duration::from_secs(10);

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
    serial: u64,
    /// The file they're kept in, while this daemon holds its lock.
    path: Option<PathBuf>,
    /// The lock, held while it's open.
    lock: Option<File>,
    /// When they're next written, as something kept has changed since
    /// they last were.
    due: Option<Instant>,
    /// What was last written, or is being, so the same isn't written
    /// again: at first, no workspaces; `None` once a write has failed, so
    /// what's there now is written, changed or not.
    last: Option<State>,
    /// The thread writing them, started with the first write.
    writer: Option<saved::Writer>,
    /// Set by the writer's thread when a write fails.
    failed: Arc<AtomicBool>,
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
    /// doing so (see [`saved::lock`]), or else nothing. It waits a moment
    /// for the lock, if it's held: starting afresh, for a daemon that has
    /// just stopped; carrying on from an upgrade, for a copy of the lock a
    /// program the build before started took, until it runs its program.
    /// Without it, the daemon would keep nothing for as long as it runs, and
    /// take nothing from what the build before kept (CLACKA-131).
    pub(super) fn new(config: &crate::DaemonConfig) -> Self {
        let Some(path) = config.workspaces.clone() else {
            return Self::default();
        };
        let patience = saved::LOCK_PATIENCE;
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
    pub(super) fn activity_storage_enabled(&self) -> bool {
        self.keeping.path.is_some()
    }
    /// Notes that something kept has changed: the workspaces are written
    /// a moment later.
    pub(super) fn persist_activity_intent(&mut self) -> Result<u64, String> {
        if self.keeping.path.is_none() {
            return Err("Managed activities need durable workspace storage enabled.".into());
        }
        self.keep();
        if self.keeping.writer.is_none() {
            return Err("The operation journal writer could not start.".into());
        }
        Ok(self.keeping.serial)
    }

    pub(super) fn keep_soon(&mut self) {
        if self.keeping.path.is_some() && self.keeping.due.is_none() {
            self.keeping.due = Some(Instant::now() + KEEP_AFTER);
        }
    }

    /// Writes the workspaces, if it's time to: a while after a write
    /// failed, too, whether they've changed since or not.
    pub(super) fn keep_if_due(&mut self) {
        if let Some(serial) = self
            .keeping
            .writer
            .as_ref()
            .and_then(saved::Writer::written)
        {
            self.activities_written(serial);
        }
        if self.write_failed() {
            self.keeping.due.get_or_insert(Instant::now() + KEEP_AGAIN);
        }
        if self.keeping.due.is_some_and(|due| due <= Instant::now()) {
            self.keeping.due = None;
            self.keep();
        }
    }

    /// Whether a write has failed since this was last asked: what was
    /// last sent to be written is then forgotten, so it's sent again.
    fn write_failed(&mut self) -> bool {
        let failed = self.keeping.failed.swap(false, Ordering::AcqRel);
        if failed {
            self.keeping.last = None;
        }
        failed
    }

    /// Has the writer's thread write the workspaces as they are now, if
    /// they've changed since they were last written, or the last write
    /// failed.
    pub(super) fn keep(&mut self) {
        let Some(path) = self.keeping.path.clone() else {
            return;
        };
        self.write_failed();
        let state = self.kept_state();
        if self.keeping.last.as_ref() == Some(&state) {
            return;
        }
        if self.keeping.writer.is_none() {
            let waker = Arc::clone(&self.waker);
            let failed = Arc::clone(&self.keeping.failed);
            let wake = move || {
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for a failed write failed: {err}");
                }
            };
            match saved::Writer::start(path, self.keeping.gate.clone(), failed, wake) {
                Ok(writer) => self.keeping.writer = Some(writer),
                Err(err) => {
                    log::error!("the workspaces can't be kept: no thread to write them: {err}");
                    return;
                }
            }
        }
        if let Some(writer) = &self.keeping.writer {
            self.keeping.serial = writer.write(state.clone());
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
        self.kept_typed_into(id);
        if let Some(pane) = self.pane_of(id)
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
            && let Some(pane) = self.pane_of(id)
            && self.workspaces.forget_ran(pane)
        {
            self.workspaces_changed();
        }
    }

    /// The workspaces, as they're kept.
    fn kept_state(&mut self) -> State {
        self.note_borrowers_seen();
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
            operations: self.saved_activities(),
            clackatoo_workspaces: saved::VERSION,
            workspaces: Vec::new(),
            active: None,
            connections: (self.links.kept().into_iter())
                .map(super::links::Kept::saved)
                .collect(),
            borrowers: (self.workspaces.borrowers().iter())
                .map(|(machine, borrower)| saved::Borrower {
                    machine: machine.to_string(),
                    name: borrower.name.clone(),
                    last_seen: borrower.last_seen,
                })
                .collect(),
            // Kept in a file of their own (see `crate::known`).
            keys: Vec::new(),
        };
        for workspace in &workspaces.list {
            let mut tabs = Vec::new();
            let mut active_tab = 0;
            for tab in &workspace.tabs {
                let Some(kept) = self.kept_tab(tab) else {
                    continue;
                };
                if tab.id == workspace.active_tab {
                    active_tab = tabs.len();
                }
                tabs.push(kept);
            }
            if tabs.is_empty() {
                continue;
            }
            let background = (workspace.background.iter())
                .filter_map(|background| self.kept_tab(&background.tab))
                .collect();
            if workspaces.active == Some(workspace.id) {
                state.active = Some(state.workspaces.len());
            }
            state.workspaces.push(saved::Workspace {
                name: workspace.name.clone(),
                tabs,
                active_tab,
                background,
                lent: workspace.lent.map(|lent| saved::Lent {
                    to: lent.to.to_string(),
                    key: lent.key,
                    placed: lent.placed,
                    background: lent.background.is_some(),
                }),
            });
        }
        state
    }

    /// `tab`, as it's kept: `None` if none of its panes is.
    fn kept_tab(&self, tab: &clackatoo_protocol::Tab) -> Option<saved::Tab> {
        let mut kept = Vec::new();
        let layout = self.kept_layout(&tab.layout, &mut kept)?;
        Some(saved::Tab {
            name: tab.name.clone(),
            layout,
            focused: (kept.iter().position(|&pane| pane == tab.focused)).unwrap_or(0),
        })
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
        if let Some(borrowed) = self.workspaces.borrowed().get(&id) {
            return Some(Pane::Terminal {
                cwd: None,
                ran: None,
                keep: None,
                elsewhere: Some(saved::Elsewhere {
                    machine: borrowed.machine.to_string(),
                    key: borrowed.key,
                    kind: borrowed.kind.to_string(),
                    name: self.workspaces.lenders().get(&borrowed.machine).cloned(),
                }),
            });
        }
        let restored = self.workspaces.restored(id);
        let kept = self.workspaces.kept(id);
        match content {
            Content::Terminal(terminal) => match &self.terminals.get(&terminal)?.terminal {
                Hosted::Pty(terminal) => {
                    let emulator = terminal.emulator();
                    Some(Pane::Terminal {
                        cwd: (terminal.own_directory())
                            .or_else(|| emulator.cwd().map(str::to_owned)),
                        // An offer still open stays open after a restart.
                        // Otherwise, what the shell's integration said it
                        // runs, with the terminal's secret, never what a
                        // program printed: or for a background tab's
                        // terminal whose shell has ended, what it ran last.
                        // One kept running is written as offered too, for
                        // a build from before then.
                        ran: (kept.map(|kept| kept.command.clone()))
                            .or_else(|| restored.and_then(|restored| restored.ran.clone()))
                            .or_else(|| {
                                match terminal.status() {
                                    TerminalStatus::Running => emulator.trusted_running_command(),
                                    _ => emulator.trusted_last_command(),
                                }
                                .and_then(kept_command)
                            }),
                        elsewhere: None,
                        keep: kept.map(|kept| saved::Keep {
                            command: kept.command.clone(),
                            always: kept.always,
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
                    started: self.kept_standing(terminal, chat),
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
                    dir: slot.buffer.dir().to_str().map(str::to_owned),
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
        // What the file holds: what's made again is written over it only
        // where it differs, even if everything is closed again before the
        // first write, which a daemon starting empty would take for no
        // change.
        self.keeping.last = Some(state.clone());
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
                let Some(made) = self.restore_tab(tab, &mut chats, &mut room, &mut restored) else {
                    continue;
                };
                if index == workspace.active_tab {
                    active_tab = tabs.len();
                }
                tabs.push(made);
            }
            let background: Vec<_> = (workspace.background.iter())
                .filter_map(|tab| self.restore_tab(tab, &mut chats, &mut room, &mut restored))
                .collect();
            made.push(if tabs.is_empty() && background.is_empty() {
                None
            } else {
                let made =
                    self.workspaces
                        .add_restored(&workspace.name, tabs, active_tab, background);
                // Lent to another machine's daemon, it still is, as that
                // daemon finds its pane again by the number it gave it.
                if let Some(lent) = workspace.lent.as_ref().and_then(restored_lent) {
                    self.workspaces.set_lent(made, lent);
                }
                Some(made)
            });
        }
        let active = (state.active)
            .and_then(|active| made.get(active).copied().flatten())
            .filter(|&active| !self.workspaces.is_lent(active))
            .or_else(|| {
                (made.iter().copied().flatten()).find(|&made| !self.workspaces.is_lent(made))
            });
        if let Some(active) = active {
            self.workspaces.set_active(active);
        }
        self.workspaces.set_restored(restored);
        self.workspaces.refresh_background();
        self.restate_backgrounds();
        self.restate_lent_backgrounds();
        self.restore_standing(&state, &chats);
        self.restore_cold_activities(state.operations, &chats);
        self.keep_restored();
        self.restore_borrowers(&state.borrowers);
        self.restore_keys(&state.keys);
        self.connect_kept(state.connections.iter().map(super::links::Kept::from_saved));
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

    /// Knows other machines by the identity keys `kept`, as a build that
    /// kept them with the workspaces did, unless they're known already;
    /// one that can't be read is passed over.
    fn restore_keys(&self, kept: &[saved::Known]) {
        for known in kept {
            let machine = known.machine.parse();
            let key = crate::machine::unhex(&known.key);
            if let (Ok(machine), Some(key)) = (machine, key) {
                let pin = super::links::Pin {
                    key,
                    met: known.met,
                };
                self.links.pins.learn(machine, pin);
            }
        }
    }

    /// Takes the machines the workspaces made again were lent to, as kept,
    /// and gives back at once those lent to one gone a long while.
    fn restore_borrowers(&mut self, kept: &[saved::Borrower]) {
        self.workspaces.set_borrowers(
            (kept.iter())
                .filter_map(|borrower| {
                    let kept = crate::workspaces::Borrower {
                        name: borrower.name.clone(),
                        last_seen: borrower.last_seen,
                    };
                    Some((borrower.machine.parse().ok()?, kept))
                })
                .collect(),
        );
        self.workspaces.refresh_lent();
        self.unlend_unseen();
    }

    /// Takes the offers to run a command again still open from the
    /// workspaces kept, as a daemon carries on from a build that couldn't
    /// hand them over, of handover version 26 or before, which would
    /// otherwise lose every one, and the next write with them
    /// (CLACKA-131). Never called after a build that could: it handed
    /// over what it had, even none.
    ///
    /// That build wrote the file as it handed over, from these same
    /// workspaces, if it could, so its panes are those written now, in
    /// the same order. A terminal it wrote as having run a command, which
    /// would be written now as having run none, and whose shell has run
    /// nothing since it started, can only have had that offer open. But a
    /// write that failed, or didn't finish in time, leaves a file older
    /// than the workspaces it handed over, which nothing in the handover
    /// dates; so nothing is taken from a file that differs in any other
    /// way from what would be written now, nor from one written before
    /// any of its terminals started. What it can't tell is an offer
    /// dismissed since its last write, when its last writes failed.
    pub(super) fn offers_kept(&mut self) {
        let Some(path) = self.keeping.path.clone() else {
            return;
        };
        let Ok(Some(kept)) = saved::read(&path) else {
            return;
        };
        let refused = |why: &str| {
            log::warn!("no offer to run a command again is taken from the workspaces kept: {why}");
        };
        let mut was = Vec::new();
        for workspace in &kept.workspaces {
            for tab in workspace.tabs.iter().chain(&workspace.background) {
                saved_panes(&tab.layout, &mut was);
            }
        }
        let panes = self.panes_kept();
        if panes.len() != was.len() {
            refused("they don't line up with those handed over");
            return;
        }
        let offers: Vec<(usize, PaneId, String)> = (panes.iter().zip(&was).enumerate())
            .filter_map(|(at, ((pane, now), was))| match (now, was) {
                (
                    Pane::Terminal {
                        ran: None,
                        keep: None,
                        elsewhere: None,
                        ..
                    },
                    Pane::Terminal {
                        ran: Some(ran),
                        keep: None,
                        elsewhere: None,
                        ..
                    },
                ) if self.ran_nothing(*pane) => Some((at, *pane, ran.clone())),
                _ => None,
            })
            .collect();
        if offers.is_empty() {
            return;
        }
        // But for those, the file is what would be written now.
        let mut unoffered = kept.workspaces;
        let offered: Vec<usize> = offers.iter().map(|&(at, ..)| at).collect();
        let mut at = 0;
        for workspace in &mut unoffered {
            for tab in workspace.tabs.iter_mut().chain(&mut workspace.background) {
                unoffer(&mut tab.layout, &mut at, &offered);
            }
        }
        if unoffered != self.kept_state().workspaces {
            refused("they're older than the workspaces handed over");
            return;
        }
        if self.started_since(&path, &panes) {
            refused("they were written before a terminal of theirs started");
            return;
        }
        let count = offers.len();
        for (_, pane, ran) in offers {
            self.workspaces.offer_again(pane, ran);
        }
        log::info!(
            "{count} offers to run a command again, which the build before couldn't hand over, \
             were taken from the workspaces kept"
        );
    }

    /// The terminal pane `pane` shows, if it's one of this machine's.
    fn pty_of(&self, pane: PaneId) -> Option<&crate::hosted::HostedTerminal> {
        let workspaces = self.workspaces.get();
        let found = (workspaces.find_pane(pane).map(|(.., found)| found)).or_else(|| {
            workspaces
                .find_background_pane(pane)
                .map(|(.., found)| found)
        });
        let Content::Terminal(terminal) = found?.content else {
            return None;
        };
        match &self.terminals.get(&terminal)?.terminal {
            Hosted::Pty(terminal) => Some(terminal),
            Hosted::Chat(_) => None,
        }
    }

    /// Whether the shell of terminal pane `pane` has run nothing since it
    /// started, as its marks say: it's at its first prompt, or has marked
    /// none, and has said it ran no command with its secret.
    fn ran_nothing(&self, pane: PaneId) -> bool {
        self.pty_of(pane).is_some_and(|terminal| {
            let emulator = terminal.emulator();
            let first = (emulator.latest_block())
                .is_none_or(|latest| latest == (0, Progress::Prompt))
                && emulator.block_renumberings() == 0;
            first && emulator.trusted_last_command().is_none()
        })
    }

    /// Whether any terminal of `panes` started after the file at `path`
    /// was last written, by more than the kernel's clocks can be trusted
    /// to say: a file written since would know of it.
    fn started_since(&self, path: &Path, panes: &[(PaneId, Pane)]) -> bool {
        /// How far apart the file's time and a program's start may be and
        /// not be told apart: the boot time is to the second.
        const SLACK: Duration = Duration::from_secs(2);
        let Ok(written) = std::fs::metadata(path).and_then(|meta| meta.modified()) else {
            return true;
        };
        (panes.iter())
            .filter_map(|&(pane, _)| self.pty_of(pane)?.started_at())
            .any(|started| started > written + SLACK)
    }

    /// Each pane that's kept, by its ID, as it's written now, in the order
    /// [`kept_state`](Self::kept_state) writes them: each workspace's tabs'
    /// panes, then its background tabs', but for workspaces none of whose
    /// tabs is kept.
    fn panes_kept(&self) -> Vec<(PaneId, Pane)> {
        let kept = |tab: &clackatoo_protocol::Tab| {
            (tab.layout.panes().into_iter())
                .filter_map(|pane| Some((pane.id, self.kept_pane(pane.id, pane.content)?)))
                .collect::<Vec<_>>()
        };
        let mut panes = Vec::new();
        for workspace in &self.workspaces.get().list {
            let tabs: Vec<(PaneId, Pane)> = workspace.tabs.iter().flat_map(kept).collect();
            if tabs.is_empty() {
                continue;
            }
            panes.extend(tabs);
            panes
                .extend((workspace.background.iter()).flat_map(|background| kept(&background.tab)));
        }
        panes
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
                started: _,
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

    /// Where the chat `id`, `chat`, stands among the chats agents
    /// started, to keep: `None` for one the user started.
    fn kept_standing(&self, id: TerminalId, chat: &HostedChat) -> Option<saved::Started> {
        if !chat.agents_chat() && chat.profile().is_none() {
            return None;
        }
        let relations = chat.relations();
        let parent = (relations.parent).filter(|&parent| self.chat_ref(parent).is_some());
        Some(saved::Started {
            parent: parent.map(|parent| parent.0),
            depth: relations.depth,
            turn: parent.and_then(|parent| Some(self.chat_ref(parent)?.subagent(id)?.turn)),
            profile: chat.profile().map(str::to_owned),
            limits: Some(chat.limits().name().to_owned()),
            kept: chat.kept_by_user(),
            mode: (chat.kept_mode()).map(|(setting, value)| saved::Mode { setting, value }),
        })
    }

    /// Makes each chat an agent started, made again in `chats` by its ID
    /// in the run that kept `state`, stand as it did: under its parent, on
    /// its parent's card, as its profile, with its limits, and the mode it
    /// chose, never more than it had nor than its parent has. A chat whose
    /// parent wasn't made again, or in another workspace, is on its own,
    /// with what it may do as it was, and says so. A
    /// chain of parents that loops, or goes further up than chats go deep,
    /// as only a file mended by hand could have, is cut.
    fn restore_standing(&mut self, state: &State, chats: &HashMap<u32, (TerminalId, bool)>) {
        let kept = kept_standings(state, chats);
        if kept.is_empty() {
            return;
        }
        // Each one's parent, by the file's IDs, if it was made again in
        // the same workspace: a chat's family reaches across workspaces,
        // which only a file mended by hand could make it do anew.
        let mut parents: HashMap<u32, u32> = (kept.iter())
            .filter_map(|(&id, started)| {
                let parent = started.parent?;
                let (&(chat, _), &(of, _)) = (chats.get(&id)?, chats.get(&parent)?);
                let workspace = self.workspace_of_chat(chat);
                (parent != id && workspace.is_some() && workspace == self.workspace_of_chat(of))
                    .then_some((id, parent))
            })
            .collect();
        let mut order: Vec<u32> = kept.keys().copied().collect();
        order.sort_unstable();
        for &id in &order {
            if above(&parents, id).is_none() {
                log::warn!(
                    "chat {id} kept: its parents go round, or too far up, so it's on its own"
                );
                parents.remove(&id);
            }
        }
        // Parents first, so each child is made to stand under its parent
        // as it stands now.
        order.sort_by_key(|id| (above(&parents, *id).unwrap_or(0), *id));
        for id in order {
            let (chat, _) = chats[&id];
            let started = kept[&id];
            let parent = parents.get(&id).map(|parent| chats[parent].0);
            let of = parent.and_then(|parent| Some((parent, self.chat_ref(parent)?)));
            let agent = self.chat_ref(chat).map(|kid| kid.agent().to_owned());
            let (root, depth, parent_limits, cap) = match of {
                Some((parent, of)) => (
                    Some(of.relations().root.unwrap_or(parent)),
                    of.relations().depth.saturating_add(1),
                    of.limits(),
                    (agent.as_deref() == Some(of.agent()))
                        .then(|| of.mode_to_be().map(str::to_owned))
                        .flatten(),
                ),
                None => (None, 1, Limits::None, None),
            };
            let limits = match started.limits.as_deref() {
                Some(name) => Limits::named(name).unwrap_or_else(|| {
                    log::warn!(
                        "chat {id} kept with limits {name:?}, which this build doesn't know: \
                         it's read-only"
                    );
                    Limits::ReadOnly
                }),
                // A profile is never made again without its limits.
                None if started.profile.is_some() => Limits::ReadOnly,
                None => Limits::None,
            };
            let standing = Standing {
                parent,
                root,
                depth: started.depth.max(depth),
                profile: started.profile.clone(),
                limits: limits.max(parent_limits),
                kept: started.kept,
                mode: (started.mode.as_ref())
                    .map(|mode| (mode.setting.clone(), mode.value.clone())),
                cap,
            };
            let alone = started.parent.is_some() && parent.is_none();
            let Some(hosted) = self.chat(chat) else {
                continue;
            };
            hosted.stand_again(standing);
            if alone {
                hosted.stand_alone();
                log::info!("chat {chat} made again on its own: the chat that started it wasn't");
                continue;
            }
            let Some(parent) = parent else {
                continue;
            };
            log::info!("chat {chat} made again as a subagent of chat {parent}");
            // Its card in its parent, on the turn it was, as neither has
            // its turns until its session is told again.
            let shown = self.workspaces.get().pane_showing(chat).is_some();
            let turn = (started.turn)
                .or_else(|| Some(self.chat_ref(parent)?.turn_at(u64::MAX)))
                .unwrap_or_default();
            let card = (self.chat_ref(chat)).map(|kid| kid.as_subagent(turn, shown));
            if let Some(card) = card
                && let Some(of) = self.chat(parent)
            {
                of.set_subagent(card);
                self.collect_damage(parent);
            }
        }
    }

    /// Makes `tab` again: its name, layout and focused pane, or `None` if
    /// none of its panes could be made, noting what each has to show for
    /// the restart in `restored`.
    fn restore_tab(
        &mut self,
        tab: &saved::Tab,
        chats: &mut HashMap<u32, (TerminalId, bool)>,
        room: &mut usize,
        restored: &mut Vec<Restored>,
    ) -> Option<(Option<String>, Layout, Option<PaneId>)> {
        let mut panes = Vec::new();
        let layout = self.restore_layout(&tab.layout, chats, room, &mut panes, restored)?;
        let focused = panes.get(tab.focused).copied().flatten();
        Some((tab.name.clone(), layout, focused))
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
            // A pane borrowed from another machine is shown again once
            // the daemon there is reached, and says what it shows.
            saved::Layout::Pane(Pane::Terminal {
                elsewhere: Some(elsewhere),
                ..
            }) => {
                let machine = elsewhere.machine.parse().ok();
                let made = machine.filter(|_| *room > 0).map(|machine| {
                    *room -= 1;
                    let kind = clackatoo_protocol::PaneKind::new(&elsewhere.kind);
                    let borrowed = Borrowed {
                        machine,
                        key: elsewhere.key,
                        kind,
                        placed: false,
                    };
                    let pane = self.workspaces.fresh_borrowed(borrowed);
                    if let Some(name) = &elsewhere.name {
                        self.workspaces.lender_named(machine, name);
                    }
                    pane
                });
                panes.push(made.map(|made| made.id));
                Some(Layout::Pane(made?))
            }
            saved::Layout::Pane(pane) => {
                let content = self.restore_pane(pane, chats, room);
                let made = content.map(|content| self.workspaces.fresh_pane(content));
                panes.push(made.map(|made| made.id));
                let made = made?;
                let (ran, place) = match pane {
                    // Kept running, it runs again, offering nothing.
                    Pane::Terminal {
                        keep: Some(keep), ..
                    } => {
                        let command = kept_command(&keep.command);
                        if let Some(command) = command {
                            self.keeps.restoring.push((made.id, command, keep.always));
                        }
                        (None, None)
                    }
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
            Pane::Editor {
                path,
                recovery,
                dir,
                ..
            } => {
                // Its changes not saved came back as a buffer of their own,
                // with no file yet, or with its file as it was, opened from
                // the directory its recovery file kept, or a file's own
                // directory if it came back with no name.
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
                // Opened from where it was, or, if that's gone or a build
                // from before then kept it, from its file's own directory:
                // never the home directory. A stat, as opening a file is.
                let dir = (dir.as_deref())
                    .filter(|dir| Path::new(dir).is_absolute() && Path::new(dir).is_dir())
                    .or_else(|| Path::new(path).parent()?.to_str());
                let file = OpenFile {
                    path: path.clone(),
                    cwd: dir.map(str::to_owned),
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

/// Who a workspace was lent to, as `lent` kept it, if it says.
fn restored_lent(lent: &saved::Lent) -> Option<clackatoo_protocol::Lent> {
    Some(clackatoo_protocol::Lent {
        to: lent.to.parse().ok()?,
        key: lent.key,
        placed: lent.placed,
        // Lent as a background tab, its new shell is said again once made.
        background: lent
            .background
            .then_some(clackatoo_protocol::LentBackground {
                state: clackatoo_protocol::BackgroundState::Running,
                failed: None,
            }),
    })
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

/// Writes the terminals of `layout`, as kept, whose places in order,
/// counted on from `at`, are among `offered`, as having run nothing.
fn unoffer(layout: &mut saved::Layout, at: &mut usize, offered: &[usize]) {
    match layout {
        saved::Layout::Split { parts, .. } => {
            for part in parts {
                unoffer(part, at, offered);
            }
        }
        saved::Layout::Pane(pane) => {
            if offered.contains(at)
                && let Pane::Terminal { ran, .. } = pane
            {
                *ran = None;
            }
            *at += 1;
        }
    }
}

/// Where each chat an agent started that's in `state`, and made again in
/// `chats`, stood, by its ID in the run that kept `state`: the first pane
/// of each, as a file mended by hand could have one in two.
fn kept_standings<'a>(
    state: &'a State,
    chats: &HashMap<u32, (TerminalId, bool)>,
) -> HashMap<u32, &'a saved::Started> {
    let mut kept = HashMap::new();
    for tab in state
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
    {
        let mut panes = Vec::new();
        saved_panes(&tab.layout, &mut panes);
        for pane in panes {
            if let Pane::Chat {
                id,
                started: Some(started),
                ..
            } = pane
                && chats.contains_key(id)
            {
                kept.entry(*id).or_insert(started);
            }
        }
    }
    kept
}

/// How many chats are above the chat `id` by `parents`, each chat's
/// parent by the saved file's IDs: `None` if they go round, or further up
/// than chats go deep.
fn above(parents: &HashMap<u32, u32>, id: u32) -> Option<u8> {
    /// Further than chats go deep (`MAX_DEPTH`), and than chats taken on
    /// by those above as their parents close can be.
    const MOST: u8 = 8;
    let mut at = id;
    for count in 0..=MOST {
        match parents.get(&at) {
            None => return Some(count),
            Some(&up) => at = up,
        }
    }
    None
}

/// The panes of `layout`, as kept, in order, from the top left, added to
/// `into`.
fn saved_panes<'a>(layout: &'a saved::Layout, into: &mut Vec<&'a Pane>) {
    match layout {
        saved::Layout::Split { parts, .. } => {
            for part in parts {
                saved_panes(part, into);
            }
        }
        saved::Layout::Pane(pane) => into.push(pane),
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
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use clackatoo_protocol::{NewTerminal, Size};

    use super::{Limits, kept_command};

    /// A copy of the lock on the workspaces a program just started took
    /// holds it a moment after the build before let it go: the next build,
    /// carrying on from an upgrade, waits for it rather than keeping
    /// nothing, and taking no offers from what was kept (CLACKA-131).
    #[test]
    fn keeping_waits_a_moment_for_a_lock_held_by_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let mut config = daemon.config.clone();
        drop(daemon);
        let path = dir.path().join("workspaces");
        config.workspaces = Some(path.clone());
        let held = crate::saved::lock(&path, std::time::Duration::ZERO).unwrap();
        let letting_go = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(held);
        });
        let keeping = super::Keeping::new(&config);
        letting_go.join().unwrap();
        assert_eq!(keeping.path, Some(path));
    }

    /// A write that fails is made again, changed or not: a while later,
    /// and as the daemon stops or upgrades, rather than taken for done
    /// once it's sent to be written (CLACKA-131).
    #[test]
    fn a_write_that_failed_is_made_again_though_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let mut config = daemon.config.clone();
        let path = dir.path().join("state/workspaces");
        config.workspaces = Some(path.clone());
        daemon.keeping = super::Keeping::new(&config);
        // Where the file is written first, a folder: every write fails.
        let blocking = dir.path().join("state/workspaces.new");
        std::fs::create_dir(&blocking).unwrap();
        let request = NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["cat".into()]),
            cwd: None,
            record_to: None,
        };
        let terminal = daemon.new_terminal(&request).unwrap();
        daemon
            .workspaces
            .new_workspace("written".to_owned(), terminal);
        daemon.keep();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !daemon.keeping.failed.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "the write never failed");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Made again a while later.
        daemon.keep_if_due();
        assert!(daemon.keeping.due.is_some());
        // Or as the daemon stops, as nothing changed meanwhile.
        std::fs::remove_dir(&blocking).unwrap();
        daemon.keep_last();
        let kept = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(kept.contains("\"written\""), "{kept}");
    }

    /// A chat pane as the saved workspaces keep it: chat `id`, called
    /// `title`, standing as `started` says, if an agent started it.
    fn kept_chat(id: u32, title: &str, started: Option<serde_json::Value>) -> serde_json::Value {
        let mut chat = serde_json::json!({
            "id": id,
            "agent": "claude",
            "session": format!("s-{id}"),
            "title": title,
            "cwd": "/",
        });
        if let Some(started) = started {
            chat["started"] = started;
        }
        serde_json::json!({ "pane": { "chat": chat } })
    }

    /// A workspace of `chats`, side by side.
    fn kept_workspace(name: &str, chats: &[serde_json::Value]) -> serde_json::Value {
        let shares = vec![1; chats.len()];
        serde_json::json!({ "name": name, "tabs": [{ "layout": { "split": {
            "left_to_right": true, "parts": chats, "shares": shares,
        }}}]})
    }

    /// A subagent made again never stands with more than it had, nor more
    /// than its parent has, whatever the file says: its limits are its
    /// parent's at least, its depth no less than its parent's and one,
    /// its mode none above its parent's, and its parent in its own
    /// workspace; a loop, or a chain longer than chats go deep, is cut.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one file, and what each chat comes back as"
    )]
    fn a_subagent_made_again_never_has_more_than_it_had_or_than_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let mut config = daemon.config.clone();
        let path = dir.path().join("workspaces");
        config.workspaces = Some(path.clone());
        daemon.keeping = super::Keeping::new(&config);
        let started = |parent: u32, depth: u8, limits: &str, mode: Option<&str>| {
            let mut started = serde_json::json!({
                "parent": parent, "depth": depth, "profile": "general", "limits": limits,
            });
            if let Some(mode) = mode {
                started["mode"] = serde_json::json!({ "setting": "mode", "value": mode });
            }
            Some(started)
        };
        let mut chain: Vec<serde_json::Value> = (50..60)
            .map(|id| kept_chat(id, &format!("chain {id}"), started(id + 1, 1, "none", None)))
            .collect();
        chain.push(kept_chat(60, "chain 60", None));
        let mut there = vec![
            kept_chat(30, "elsewhere", started(20, 1, "none", Some("default"))),
            kept_chat(40, "loop 40", started(41, 1, "none", None)),
            kept_chat(41, "loop 41", started(40, 1, "none", None)),
        ];
        there.extend(chain);
        let state = serde_json::json!({
            "clackatoo_workspaces": 1,
            "workspaces": [
                kept_workspace("here", &[
                    kept_chat(20, "user", None),
                    // Kept too shallow, in a mode above its parent's.
                    kept_chat(21, "sub", started(20, 0, "read-only", Some("bypassPermissions"))),
                    // Kept with fewer limits than its parent's.
                    kept_chat(22, "grand", started(21, 1, "none", Some("acceptEdits"))),
                    // In a mode below its parent's, which it keeps.
                    kept_chat(23, "edits", started(20, 1, "none", Some("acceptEdits"))),
                ]),
                kept_workspace("there", &there),
            ],
        });
        std::fs::write(&path, state.to_string()).unwrap();
        daemon.restore_workspaces();
        let id = |title: &str| {
            (daemon.terminals.iter())
                .find(|(_, slot)| {
                    (slot.terminal.chat_ref()).is_some_and(|chat| chat.info().title == title)
                })
                .map_or_else(|| panic!("no chat {title}"), |(&id, _)| id)
        };
        let chat = |title: &str| daemon.chat_ref(id(title)).unwrap();
        let stands = |title: &str| {
            let chat = chat(title);
            let parent = chat.relations().parent;
            (
                parent,
                chat.relations().depth,
                chat.limits(),
                chat.mode_to_be(),
            )
        };
        let (user, sub) = (id("user"), id("sub"));
        // Claude Code's new chats start in Auto, which the user's comes
        // back in, as before.
        assert_eq!(chat("user").mode_to_be(), Some("auto"));
        assert_eq!(
            stands("sub"),
            (Some(user), 1, Limits::ReadOnly, Some("default"))
        );
        assert_eq!(
            stands("grand"),
            (Some(sub), 2, Limits::ReadOnly, Some("default"))
        );
        assert_eq!(
            stands("edits"),
            (Some(user), 1, Limits::None, Some("acceptEdits"))
        );
        assert_eq!(
            chat("sub").kept_mode(),
            Some(("mode".to_owned(), "default".to_owned()))
        );
        // Each on its parent's card.
        assert!(
            chat("user").subagent(sub).is_some() && chat("sub").subagent(id("grand")).is_some()
        );
        // A parent in another workspace isn't its parent again: it's on
        // its own, says so, and its mode is the safest.
        assert_eq!(
            stands("elsewhere"),
            (None, 1, Limits::None, Some("default"))
        );
        let notice = chat("elsewhere").node().notice.clone().unwrap_or_default();
        assert!(notice.contains("didn't come back with it"), "{notice}");
        // A loop is cut once, in the order of the file's IDs.
        assert_eq!(chat("loop 40").relations().parent, None);
        assert_eq!(chat("loop 41").relations().parent, Some(id("loop 40")));
        // A chain longer than chats go deep is cut where it's too long.
        assert_eq!(chat("chain 50").relations().parent, None);
        assert_eq!(chat("chain 51").relations().parent, None);
        assert_eq!(chat("chain 52").relations().parent, Some(id("chain 53")));
        assert_eq!(chat("chain 59").relations().parent, Some(id("chain 60")));
    }

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
