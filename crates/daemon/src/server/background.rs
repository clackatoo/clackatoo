//! Background tabs (CLACKA-121): tabs of terminals a workspace keeps
//! running out of its tab bar, such as a web server's.
//!
//! Where they are is the workspaces' (see [`crate::workspaces`]). Here,
//! the requests about them ([`Request::Background`]), and how each is
//! getting on ([`Background::state`]): running while a command runs in one
//! of its terminals, as their shells' marks say, or while a terminal
//! whose shell doesn't mark its commands is alive; idle while its shells
//! are at their prompts; ended once its terminals' programs have, which
//! leaves it, and its last screen, until it's dismissed. A command that
//! fails, or a shell that exits with a failure, is noted as its
//! [`Background::failed`], for windows to show until one says it was seen.
//!
//! What a background tab's terminals print is looked at as it's read
//! ([`Daemon::background_moved_on`]), which for any other terminal costs
//! a lookup in a map: only a change of how a tab is getting on tells the
//! windows, never output alone.
//!
//! A command is started in a new background tab ([`BackgroundAction::Start`])
//! as a new shell with the command typed at its first prompt, as the user
//! would type it, so its shell's own marks say when it runs and how it
//! ends. The ports background tabs' programs listen on are found on a
//! thread of their own (see [`crate::ports`]), only when a window asks,
//! as its list of them is open: nothing is looked for otherwise.
//!
//! [`Request::Background`]: clackatoo_protocol::Request::Background
//! [`Background::state`]: clackatoo_protocol::Background::state
//! [`Background::failed`]: clackatoo_protocol::Background::failed

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    BackgroundAction, BackgroundState, Error, Listening, ListeningPort, NewTerminal, PaneId,
    PaneTerminal, Response, Size, Tab, TabId, TerminalId, TerminalStatus, WorkspaceId,
};
use clackatoo_term::{Progress, Terminal};
use mio::Token;

use super::Daemon;

/// The longest name a background tab is given, in characters.
const LONGEST_NAME: usize = 100;

/// `name`, safe to show as a background tab's: one line, with nothing that
/// goes unseen or turns the text round, of [`LONGEST_NAME`] characters at
/// most.
pub(super) fn shown_name(name: &str) -> String {
    let name: String = (name.chars())
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .filter(|&ch| !super::tools::invisible(ch))
        .take(LONGEST_NAME)
        .collect();
    name.trim().to_owned()
}

/// The most of a terminal's blocks looked at for failures at once: far
/// more than finish between two reads of its output.
const MOST_BLOCKS: u32 = 64;

/// How often a change of how background tabs are getting on, which the
/// marks programs print drive, is told: at most once in so long, so a
/// program printing them over and over can't flood the windows, nor the
/// disk the workspaces are kept on.
const RESTATE_EVERY: Duration = Duration::from_millis(250);

/// What the daemon keeps of each terminal in a background tab, besides
/// the workspaces.
#[derive(Debug, Default)]
pub(super) struct Backgrounds {
    /// The first of each terminal's blocks not yet looked at for a
    /// failure: one before it went to the background is never told.
    checked: HashMap<TerminalId, u32>,
    /// What finds the ports background tabs listen on, once asked.
    ports: Option<crate::ports::Finder>,
    /// The clients waiting for the ports, which they're sent once found.
    waiting: Vec<Token>,
    /// Whether the ports are being looked for.
    scanning: bool,
    /// When a change of how background tabs are getting on was last told.
    restated: Option<Instant>,
    /// When one waiting to be told is, if one is.
    pub(super) restate_due: Option<Instant>,
    /// The ports this daemon's background tabs listen on, as last found.
    pub(super) found: Vec<Listening>,
}

impl Daemon {
    /// Answers [`Request::Background`](clackatoo_protocol::Request::Background).
    pub(super) fn serve_background(
        &mut self,
        token: Token,
        action: &BackgroundAction,
    ) -> Result<Response, Error> {
        match *action {
            BackgroundAction::Start {
                workspace,
                ref command,
                ref cwd,
                pane,
                size,
            } => {
                let command = super::lent_background::checked_command(command)?;
                let (pane, terminal) =
                    self.start_background(workspace, (cwd.clone(), pane), size, command)?;
                self.type_at_first_prompt(terminal, command.to_owned(), None, None);
                Ok(Response::PaneCreated(pane))
            }
            BackgroundAction::Ports => {
                self.find_ports(token);
                Ok(Response::Done)
            }
            BackgroundAction::Tab { tab } => self.send_tab_to_background(tab),
            BackgroundAction::Pane { pane } => self.send_pane_to_background(pane),
            BackgroundAction::Bring { tab } => self.bring_back(tab),
            BackgroundAction::Dismiss { tab } => self.dismiss_background(tab),
            BackgroundAction::Seen { workspace } => {
                // Those another machine lends say so there.
                let borrowed: Vec<(PaneId, crate::workspaces::Borrowed)> =
                    (self.workspaces.get().get(workspace).into_iter())
                        .flat_map(|shown| &shown.background)
                        .filter(|background| background.failed.is_some())
                        .flat_map(|background| {
                            self.workspaces.background_borrowed(background.tab.id)
                        })
                        .collect();
                for (_, borrowed) in borrowed {
                    let seen = clackatoo_protocol::LentBackgroundAction::Seen;
                    let _ = self.ask_lent_background(borrowed, seen);
                }
                if self.workspaces.seen(workspace)? {
                    self.workspaces_changed();
                }
                Ok(Response::Done)
            }
        }
    }

    /// Sends `tab` to the background, making a tab in its place first if
    /// it's its workspace's only one.
    fn send_tab_to_background(&mut self, tab: TabId) -> Result<Response, Error> {
        let (workspace, only) = self.workspaces.check_background_tab(tab)?;
        let found = (self.workspaces.get().find_tab(tab)).map(|(_, tab)| tab.clone());
        let Some(found) = found else {
            return Err(Error::NoSuchTab(tab));
        };
        self.no_chats(&found.terminals())?;
        if only {
            self.tab_in_place(workspace, found.focused)?;
        }
        self.watch_in_background(&found);
        let (state, name) = (self.background_state(&found), self.background_name(&found));
        self.workspaces.background_tab(tab, state, name);
        self.workspaces_changed();
        Ok(Response::Done)
    }

    /// Takes `pane` out of its tab into a background tab of its own, as
    /// [`send_tab_to_background`](Self::send_tab_to_background) sends its
    /// tab if it's its only pane.
    fn send_pane_to_background(&mut self, pane: PaneId) -> Result<Response, Error> {
        let (tab, only) = self.workspaces.check_background_pane(pane)?;
        if only {
            return self.send_tab_to_background(tab);
        }
        if let Some(terminal) = self.terminal_of_pane(pane) {
            self.no_chats(&[terminal])?;
        }
        let Some((_, _, found)) = self.workspaces.get().find_pane(pane) else {
            return Err(Error::NoSuchPane(pane));
        };
        let alone = Tab {
            id: tab,
            layout: clackatoo_protocol::Layout::Pane(found),
            focused: pane,
            name: None,
        };
        self.watch_in_background(&alone);
        let (state, name) = (self.background_state(&alone), self.background_name(&alone));
        if self.workspaces.background_pane(pane, state, name).is_none() {
            return Err(Error::Other("no more tabs can be made".into()));
        }
        self.workspaces_changed();
        Ok(Response::Done)
    }

    /// A new tab in `workspace`, a shell in the directory `beside` is in,
    /// for one going to the background to leave in its place.
    fn tab_in_place(
        &mut self,
        workspace: clackatoo_protocol::WorkspaceId,
        beside: PaneId,
    ) -> Result<(), Error> {
        let size = self.pane_size(beside);
        let content = self.pane_content(
            PaneTerminal::New(NewTerminal {
                size,
                command: None,
                cwd: None,
                record_to: None,
            }),
            Some(beside),
        )?;
        self.workspaces.new_tab(workspace, content);
        Ok(())
    }

    /// The size of the terminal `pane` shows, for a new one beside it.
    fn pane_size(&self, pane: PaneId) -> Size {
        let terminal =
            (self.workspaces.check_pane(pane).ok()).and_then(clackatoo_protocol::Content::terminal);
        (terminal.and_then(|terminal| self.terminals.get(&terminal))).map_or_else(
            || Size::new(80, 24).expect("a size"),
            |slot| slot.terminal.size(),
        )
    }

    /// Brings background tab `tab` back into its workspace's tab bar. Each
    /// of its terminals whose program has ended becomes a new shell in its
    /// directory, offering to run again what it ran.
    fn bring_back(&mut self, tab: TabId) -> Result<Response, Error> {
        if self.workspaces.get().find_background(tab).is_none() {
            return Err(Error::NoSuchTab(tab));
        }
        // A shell another machine lends is brought back only as that
        // machine hears it is, or it would go on keeping it once it ends.
        for (_, borrowed) in self.workspaces.background_borrowed(tab) {
            self.can_ask_lender(borrowed)?;
        }
        let focused = self.workspaces.bring_back(tab)?;
        self.renew_ended(tab);
        self.workspaces_changed();
        Ok(Response::PaneCreated(focused))
    }

    /// Makes again each terminal of tab `tab`, just brought into its
    /// workspace's tab bar, whose program has ended: a new shell in its
    /// directory, offering to run again what it ran, or running it, if
    /// it's kept running always. A restart one was waiting for is dropped:
    /// this is it.
    pub(super) fn renew_ended(&mut self, tab: TabId) {
        // One another machine lends, that machine makes again, as it hears
        // it's brought back (CLACKA-122).
        let borrowed = self.workspaces.borrowed_in_tab(tab);
        if !borrowed.is_empty() {
            // Named after its program again, as any tab.
            self.workspaces.forget_tab_name(tab);
        }
        for (_, borrowed) in borrowed {
            let bring = clackatoo_protocol::LentBackgroundAction::Bring;
            if let Err(err) = self.ask_lent_background(borrowed, bring) {
                log::warn!("a background tab another machine lends: {err}");
            }
        }
        let Some((_, found)) = self.workspaces.get().find_tab(tab) else {
            return;
        };
        let ended: Vec<(PaneId, TerminalId)> = (found.layout.panes().into_iter())
            .filter_map(|pane| Some((pane.id, pane.content.terminal()?)))
            .filter(|&(_, terminal)| {
                (self.terminals.get(&terminal))
                    .is_none_or(|slot| slot.terminal.status() != TerminalStatus::Running)
            })
            .collect();
        // Only those made again here: the others, still running, keep how
        // running their commands again goes, a pause too.
        let renewed: Vec<PaneId> = ended.iter().map(|&(pane, _)| pane).collect();
        self.drop_restarts(&renewed);
        for (pane, old) in ended {
            if let Some(new) = self.start_again_in(pane, old)
                && self.runs_again(pane)
            {
                self.run_kept(pane, new);
            }
        }
    }

    /// Refuses to send `terminals` to the background if one is a chat:
    /// only terminals go there.
    fn no_chats(&self, terminals: &[TerminalId]) -> Result<(), Error> {
        let chat = (terminals.iter()).any(|terminal| {
            (self.terminals.get(terminal)).is_some_and(|slot| slot.terminal.grid().is_none())
        });
        if chat {
            return Err(Error::Other(
                "only terminals of this machine's own go to the background".into(),
            ));
        }
        Ok(())
    }

    /// Shows a new shell in `pane`, in a tab or a background tab, in place
    /// of terminal `old`, whose program has ended, in the directory it was
    /// in; and closes `old`, and whatever else the pane showed. Unless the
    /// pane runs its command again by itself, the new shell offers to run
    /// again the last command `old`'s shell said it ran. Returns the new
    /// shell's terminal.
    pub(super) fn start_again_in(&mut self, pane: PaneId, old: TerminalId) -> Option<TerminalId> {
        let (cwd, ran, size) = match self.terminals.get(&old) {
            Some(slot) => {
                let grid = slot.terminal.grid();
                let cwd = grid.and_then(Terminal::cwd).map(str::to_owned);
                let ran = (grid.and_then(Terminal::trusted_last_command))
                    .and_then(super::restore::kept_command);
                (cwd, ran, slot.terminal.size())
            }
            None => (None, None, Size::new(80, 24).expect("a size")),
        };
        let cwd = cwd.filter(|cwd| Path::new(cwd).is_dir());
        let request = NewTerminal {
            size,
            command: None,
            cwd,
            record_to: None,
        };
        let (new, replaced) = match self.new_terminal(&request) {
            Ok(new) => {
                let replaced = self.workspaces.replace_terminal(pane, new);
                // Its pane has gone meanwhile: so does the new shell.
                if replaced.is_none() && self.close_terminal(new).is_err() {
                    log::warn!("terminal {new} couldn't be closed");
                }
                (replaced.map(|_| new), replaced)
            }
            Err(err) => {
                log::warn!("pane {pane}: a new shell couldn't be started: {err}");
                (None, None)
            }
        };
        if let Some(ran) = ran.filter(|_| new.is_some() && !self.runs_again(pane)) {
            self.workspaces.offer_again(pane, ran);
        }
        // What the pane showed, which may be another terminal than `old`,
        // goes, and `old`, unless a pane shows it.
        for gone in replaced.into_iter().chain([old]) {
            if Some(gone) != new
                && self.terminals.contains_key(&gone)
                && self.pane_of(gone).is_none()
                && self.close_terminal(gone).is_err()
            {
                log::warn!("terminal {gone} couldn't be closed");
            }
        }
        new
    }

    /// Closes background tab `tab`, and its terminals.
    fn dismiss_background(&mut self, tab: TabId) -> Result<Response, Error> {
        // A shell another machine lends goes there, as its pane goes here.
        let borrowed = self.workspaces.background_borrowed(tab);
        if !borrowed.is_empty() {
            for &(_, lender) in &borrowed {
                self.can_ask_lender(lender)?;
            }
            for (pane, _) in borrowed {
                self.workspaces.remove_pane(pane);
            }
            self.workspaces_changed();
            return Ok(Response::Done);
        }
        let Some((_, background)) = self.workspaces.get().find_background(tab) else {
            return Err(Error::NoSuchTab(tab));
        };
        let panes: Vec<PaneId> = (background.tab.layout.panes().iter())
            .map(|pane| pane.id)
            .collect();
        let terminals = background.tab.terminals();
        self.drop_restarts(&panes);
        for terminal in terminals {
            self.close_terminal(terminal)?;
        }
        self.workspaces_changed();
        Ok(Response::Done)
    }

    /// Starts a shell in a new background tab of `workspace`, in `cwd`, or
    /// else in the directory `beside` is in, or the workspace's focused
    /// pane, as a new pane beside it would start, called after `command`,
    /// which is to run in it. Returns its pane and its terminal, for the
    /// command to be typed at its first prompt.
    pub(super) fn start_background(
        &mut self,
        workspace: WorkspaceId,
        (cwd, beside): (Option<String>, Option<PaneId>),
        size: Size,
        command: &str,
    ) -> Result<(PaneId, TerminalId), Error> {
        self.workspaces.check_new_background(workspace)?;
        let beside = beside.or_else(|| self.workspaces.focused_pane(workspace));
        // A pane on another machine has its own directories.
        if beside.is_some_and(|pane| self.workspaces.borrowed().contains_key(&pane)) {
            return Err(Error::Other(
                "a pane on another machine starts its background tabs there, through the daemon \
                 connected to it"
                    .into(),
            ));
        }
        let cwd = cwd.or_else(|| beside.and_then(|pane| self.working_directory(pane)));
        let terminal = self.new_terminal(&NewTerminal {
            size,
            command: None,
            cwd,
            record_to: None,
        })?;
        let (tab, pane) = self.workspaces.new_background(workspace, terminal.into());
        if let Some((_, background)) = self.workspaces.get().find_background(tab) {
            let tab = background.tab.clone();
            self.watch_in_background(&tab);
            let state = self.background_state(&tab);
            if let Some(background) = self.workspaces.background_mut(tab.id) {
                background.state = state;
                background.name = shown_name(command);
            }
        }
        self.workspaces_changed();
        Ok((pane, terminal))
    }

    /// Looks for the ports background tabs' programs listen on, for the
    /// client at `token`, which is sent them once they're found: started
    /// now unless a look is under way, which it's sent instead.
    fn find_ports(&mut self, token: Token) {
        if !self.background.waiting.contains(&token) {
            self.background.waiting.push(token);
        }
        if self.background.scanning {
            return;
        }
        let mut asked = Vec::new();
        for background in (self.workspaces.get().list.iter()).flat_map(|w| &w.background) {
            for terminal in background.tab.terminals() {
                let Some(slot) = self.terminals.get(&terminal) else {
                    continue;
                };
                if slot.terminal.status() != TerminalStatus::Running {
                    continue;
                }
                // Its shell leads a session of its own.
                if let Some(pid) = slot.terminal.info().pid {
                    asked.push((background.tab.id.0, pid));
                }
            }
        }
        if self.background.ports.is_none() {
            let proc = std::path::PathBuf::from("/proc");
            self.background.ports =
                crate::ports::Finder::start(proc, std::sync::Arc::clone(&self.waker));
        }
        // A thread that has gone is started again at the next ask.
        self.background.scanning =
            (self.background.ports.as_ref()).is_some_and(|finder| finder.ask(asked));
        if !self.background.scanning {
            self.background.ports = None;
            self.background.waiting.clear();
        }
    }

    /// Sends the ports found, if they've been, to the clients waiting for
    /// them: every background tab, those listening on none with none.
    pub(super) fn take_ports(&mut self) {
        let Some(found) = (self.background.ports.as_ref()).and_then(crate::ports::Finder::take)
        else {
            return;
        };
        self.background.scanning = false;
        let mut listening: Vec<Listening> = (self.workspaces.get().list.iter())
            .flat_map(|workspace| &workspace.background)
            .map(|background| Listening {
                tab: background.tab.id,
                ports: Vec::new(),
            })
            .collect();
        for (tab, ports) in found {
            let Some(shown) = listening.iter_mut().find(|shown| shown.tab.0 == tab) else {
                continue;
            };
            for (port, program) in ports {
                if !shown.ports.iter().any(|known| known.port == port) {
                    shown.ports.push(ListeningPort { port, program });
                }
            }
            shown.ports.sort_by_key(|port| port.port);
        }
        // Told with what other machines last said of theirs (CLACKA-122).
        self.background.found = listening;
        let waiting = std::mem::take(&mut self.background.waiting);
        self.tell_ports(waiting);
    }

    /// Starts looking at `tab`'s terminals' blocks for failures from
    /// those to come.
    pub(super) fn watch_in_background(&mut self, tab: &Tab) {
        for terminal in tab.terminals() {
            let next = (self.terminals.get(&terminal))
                .and_then(|slot| slot.terminal.grid())
                .and_then(Terminal::latest_block)
                .map_or(0, |(block, progress)| match progress {
                    Progress::Finished(_) => block.wrapping_add(1),
                    Progress::Prompt | Progress::Running => block,
                });
            self.background.checked.insert(terminal, next);
        }
    }

    /// Notes that terminal `id`'s output moved on, if it's in a background
    /// tab: a failure, or a change of how the tab is getting on, tells the
    /// windows. Costs a lookup for any other terminal.
    pub(super) fn background_moved_on(&mut self, id: TerminalId) {
        let Some(tab) = self.workspaces.background_of(id) else {
            // Or lent another daemon as one of its own.
            return self.lent_background_moved_on(id, false);
        };
        let failed = self.failure_since(id);
        self.restate_background(tab, failed);
    }

    /// Notes that terminal `id`'s program ended, if it's in a background
    /// tab: an exit with a failure is noted as one.
    pub(super) fn background_ended(&mut self, id: TerminalId) {
        let Some(tab) = self.workspaces.background_of(id) else {
            return self.lent_background_moved_on(id, true);
        };
        let failed = self.failure_since(id).or_else(|| {
            match self.terminals.get(&id)?.terminal.status() {
                TerminalStatus::Exited(0) | TerminalStatus::Running => None,
                TerminalStatus::Exited(code) => Some(code),
                // As shells say a command a signal ended.
                TerminalStatus::Killed(signal) => Some(128 + signal),
            }
        });
        self.restate_background(tab, failed);
    }

    /// Says how background tab `tab` is getting on now, with `failed` the
    /// exit status of a failure just seen, telling the windows if that's
    /// changed.
    fn restate_background(&mut self, tab: TabId, failed: Option<i32>) {
        let Some((_, background)) = self.workspaces.get().find_background(tab) else {
            return;
        };
        let state = self.background_state(&background.tab);
        if state == background.state
            && failed.is_none_or(|failed| background.failed == Some(failed))
        {
            return;
        }
        let name = self.background_name(&background.tab);
        if let Some(background) = self.workspaces.background_mut(tab) {
            background.state = state;
            background.name = name;
            if failed.is_some() {
                background.failed = failed;
            }
        }
        self.background_restated();
    }

    /// Tells the windows how background tabs are getting on now, unless
    /// that was told less than [`RESTATE_EVERY`] ago: then once it's been.
    pub(super) fn background_restated(&mut self) {
        let now = Instant::now();
        match self.background.restated {
            Some(last) if now < last + RESTATE_EVERY => {
                self.background
                    .restate_due
                    .get_or_insert(last + RESTATE_EVERY);
            }
            _ => {
                self.background.restated = Some(now);
                self.workspaces_changed();
            }
        }
    }

    /// Tells the windows how background tabs are getting on, if a change
    /// waited to be told and its time has come.
    pub(super) fn tell_restated(&mut self) {
        let now = Instant::now();
        if self.background.restate_due.is_some_and(|due| due <= now) {
            self.background.restated = Some(now);
            self.workspaces_changed();
        }
    }

    /// When a change of how background tabs are getting on is to be told,
    /// if one waits.
    pub(super) fn next_restate_due(&self) -> Option<Instant> {
        self.background.restate_due
    }

    /// Forgets what's kept of terminals no longer in background tabs, as
    /// the workspaces change: those brought back, closed, or gone with
    /// their workspace.
    pub(super) fn forget_backgrounds_gone(&mut self) {
        let workspaces = &self.workspaces;
        (self.background.checked).retain(|terminal, _| {
            workspaces.background_of(*terminal).is_some()
                || (workspaces.lent_terminal(*terminal))
                    .is_some_and(|lent| lent.background.is_some())
        });
        self.background.restate_due = None;
    }

    /// The exit status of the latest command of terminal `id`'s that
    /// failed since its blocks were last looked at, if one did.
    pub(super) fn failure_since(&mut self, id: TerminalId) -> Option<i32> {
        let grid = self.terminals.get(&id)?.terminal.grid()?;
        let (latest, _) = grid.latest_block()?;
        let checked = self.background.checked.entry(id).or_insert(latest);
        // Numbered again from 0 since.
        if *checked > latest.saturating_add(1) {
            *checked = latest;
        }
        let mut failed = None;
        let mut block = *checked;
        while block <= latest && block - *checked < MOST_BLOCKS {
            match grid.block_progress(block) {
                Some(Progress::Finished(exit)) => {
                    if let Some(code) = exit.filter(|&code| code != 0) {
                        failed = Some(code);
                    }
                }
                Some(Progress::Prompt | Progress::Running) => break,
                // Forgotten: no row is marked with it any more.
                None => {}
            }
            block += 1;
        }
        *checked = block;
        failed
    }

    /// How `tab`'s terminals are getting on, together.
    pub(super) fn background_state(&self, tab: &Tab) -> BackgroundState {
        let mut alive = false;
        for terminal in tab.terminals() {
            let Some(slot) = self.terminals.get(&terminal) else {
                continue;
            };
            let Some(grid) = slot.terminal.grid() else {
                continue;
            };
            if slot.terminal.status() != TerminalStatus::Running {
                continue;
            }
            alive = true;
            match grid.latest_block() {
                Some((_, Progress::Running)) | None => return BackgroundState::Running,
                Some((_, Progress::Prompt | Progress::Finished(_))) => {}
            }
        }
        if alive {
            return BackgroundState::Idle;
        }
        let focused = (tab.layout.panes().into_iter())
            .find(|pane| pane.id == tab.focused)
            .and_then(|pane| pane.content.terminal());
        let status = (focused.and_then(|terminal| self.terminals.get(&terminal)))
            .map_or(TerminalStatus::Exited(0), |slot| slot.terminal.status());
        BackgroundState::Ended(status)
    }

    /// What to call `tab`, as a tab is called after its focused pane: its
    /// terminal's title, or else the command its shell runs or ran last,
    /// or else its directory, from `~`.
    pub(super) fn background_name(&self, tab: &Tab) -> String {
        let focused = (tab.layout.panes().into_iter())
            .find(|pane| pane.id == tab.focused)
            .and_then(|pane| pane.content.terminal());
        let grid = (focused.and_then(|terminal| self.terminals.get(&terminal)))
            .and_then(|slot| slot.terminal.grid());
        let Some(grid) = grid else {
            return String::new();
        };
        let title = grid.title().trim();
        let name = if title.is_empty() {
            (grid
                .running_command()
                .or_else(|| grid.trusted_last_command()))
            .map(str::to_owned)
            .or_else(|| {
                let cwd = grid.cwd()?;
                Some(super::tools::homely(Path::new(cwd), &self.config.home))
            })
            .unwrap_or_default()
        } else {
            title.to_owned()
        };
        shown_name(&name)
    }

    /// Says how each background tab is getting on, as a daemon carrying
    /// on from an upgrade, which hands over only their failures, starts.
    pub(super) fn restate_backgrounds(&mut self) {
        // One another machine lends is as that machine says.
        let tabs: Vec<Tab> = (self.workspaces.get().list.iter())
            .flat_map(|workspace| &workspace.background)
            .filter(|background| {
                self.workspaces
                    .background_borrowed(background.tab.id)
                    .is_empty()
            })
            .map(|background| background.tab.clone())
            .collect();
        for tab in tabs {
            self.watch_in_background(&tab);
            let (state, name) = (self.background_state(&tab), self.background_name(&tab));
            if let Some(background) = self.workspaces.background_mut(tab.id) {
                background.state = state;
                background.name = name;
            }
        }
    }
}
