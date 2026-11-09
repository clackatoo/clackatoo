//! Terminals' history kept across a restart, in the event loop: noting
//! which terminals' lines changed, handing a copy of each to the thread
//! that writes them every so often, and once more as the daemon stops,
//! upgrades or the machine shuts down; and, as the daemon starts afresh,
//! putting what was kept back above each new shell (see
//! [`crate::history`]).
//!
//! The loop only copies: the newest pages of a terminal's history, as
//! they are, and its screen's rows. Making them plain data, compressing
//! and syncing them are all the thread's. A terminal whose output hasn't
//! paused, its newest rows not compacted yet, is put off a few rounds
//! rather than copied raw.
//!
//! What's kept: each terminal of the user's, in tabs and background tabs,
//! never an agent's or one showing what an agent's command printed. The
//! user can mark a terminal's history not to be kept, with the layout
//! (`:keep history off`), keep a block of it all the same, and forget
//! what's kept of it so far (`:forget`). Lines a program said never to
//! write to disk (see [`clackatoo_term::hints`]) aren't. A terminal closed
//! for good takes its file with it.
//!
//! After a restart, what was kept is history above the new shell's first
//! prompt, ending in a line that says when the restart was, which windows
//! draw as a line across the pane, dimming what's above. Nothing in it is
//! ever run or given to the shell.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use clackatoo_protocol::{Error, HistoryKeeping, PaneId, Response, TerminalId};
use clackatoo_term::{Hints, Keeping, KeptHistory, Which};
use mio::Token;

use super::{Daemon, Hosted};
use crate::history::{self, Job, Writer};
use crate::{HistoryConfig, handover, saved};

/// How many times a terminal whose output hasn't paused is put off,
/// before its newest rows are compacted for it to be copied anyway.
const MOST_PUT_OFF: u8 = 4;

/// The most terminals whose history is copied for the thread in one turn
/// of the loop, unless every one is wanted at once.
const MOST_A_TURN: usize = 8;

/// How soon a change the user asked of a terminal's history file, which
/// couldn't be made, is tried again.
const RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// The most threads reading history files as the daemon starts.
const MOST_READERS: usize = 8;

/// Keeping terminals' history.
#[derive(Debug, Default)]
pub(super) struct Histories {
    /// The thread writing the files, from the first write.
    writer: Option<Writer>,
    /// What's kept of terminals beyond their lines, of those with
    /// anything to say.
    marks: HashMap<TerminalId, Marks>,
    /// The terminals whose lines changed since they were last written,
    /// each with how many times it has been put off.
    changed: HashMap<TerminalId, u8>,
    /// When those are next written.
    due: Option<Instant>,
    /// History read as the daemon starts, by the name of its file, for
    /// the terminals made again to take.
    restoring: HashMap<String, KeptHistory>,
    /// The terminals whose history isn't kept, as they're an agent's,
    /// whose file has been removed since it was last written: so each is
    /// removed once, whenever a terminal stops being one kept.
    removed: HashSet<TerminalId>,
}

/// What's kept of a terminal's history beyond its lines. The blocks the
/// user kept though it's marked off are marked on their rows, which carry
/// the mark wherever their text goes (`Hints::KEPT`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Marks {
    /// Its history isn't kept, but for the blocks kept: the user marked
    /// it so.
    off: bool,
    /// The first line kept: those before were forgotten.
    from: u64,
    /// How the terminal's lines were numbered as `from` was set: its
    /// history's generation and its width. Numbered again since, `from`
    /// may name another line, so everything there was then is forgotten.
    numbered: (u64, usize),
}

impl Marks {
    /// Which lines are kept.
    fn keeping(&self) -> Keeping {
        Keeping {
            from: self.from,
            marked_only: self.off,
        }
    }

    /// Makes sure `from` still holds back everything forgotten in
    /// `terminal`, whose lines may have been numbered again since: by a
    /// reset, or a resize that rewrapped them. If they were, all there is
    /// now is forgotten, which holds back more, never less.
    fn follow(&mut self, terminal: &clackatoo_term::Terminal) {
        let numbered = numbering(terminal);
        if self.from > 0 && self.numbered != numbered {
            let primary = terminal.primary();
            self.from = primary.scrollback_end() + primary.height() as u64;
            self.numbered = numbered;
        }
    }
}

/// How `terminal`'s main screen's lines are numbered now: its history's
/// generation, which changes as they're numbered afresh, and its width,
/// which rewraps them.
fn numbering(terminal: &clackatoo_term::Terminal) -> (u64, usize) {
    let primary = terminal.primary();
    (primary.history_generation(), primary.width())
}

/// The time now, in seconds since 1970.
fn now_seconds() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// What the line ending history kept from before a restart at `at` says,
/// in the local time zone: for a window older than protocol 146, which
/// shows it as it is, and an agent reading the terminal.
fn divider(at: i64) -> String {
    let when = jiff::Timestamp::from_second(at)
        .map(|at| at.to_zoned(jiff::tz::TimeZone::system()))
        .map_or_else(
            |_| String::new(),
            |at| at.strftime(" %-d %b %H:%M").to_string(),
        );
    format!("── restarted{when} ──")
}

impl Daemon {
    /// How terminals' history is kept, while this daemon keeps the
    /// workspaces: `None` if it isn't.
    fn history_config(&self) -> Option<&HistoryConfig> {
        self.config
            .history
            .as_ref()
            .filter(|_| self.keeping_workspaces())
    }

    /// The thread writing history files, started if it isn't yet.
    fn history_writer(&mut self) -> Option<&Writer> {
        if self.histories.writer.is_none() {
            let config = self.history_config()?;
            match Writer::start(config.dir.clone(), config.bytes) {
                Ok(writer) => self.histories.writer = Some(writer),
                Err(err) => {
                    log::error!(
                        "terminals' history can't be kept in {}: {err}",
                        config.dir.display()
                    );
                    self.config.history = None;
                    return None;
                }
            }
        }
        self.histories.writer.as_ref()
    }

    /// Whether terminal `id`'s history is kept at all: a terminal of the
    /// user's, never an agent's, nor one showing what an agent's command
    /// printed.
    fn history_kept_of(&self, id: TerminalId) -> bool {
        match self.terminals.get(&id).map(|slot| &slot.terminal) {
            Some(Hosted::Pty(terminal)) => {
                !terminal.is_fed() && !terminal.for_agents() && !self.agents_terminal(id)
            }
            _ => false,
        }
    }

    /// Notes that terminal `id`'s lines changed: they're written within
    /// [`HistoryConfig::every`].
    pub(super) fn history_moved_on(&mut self, id: TerminalId) {
        let Some(every) = self.config.history.as_ref().map(|config| config.every) else {
            return;
        };
        self.histories.changed.entry(id).or_insert(0);
        self.histories
            .due
            .get_or_insert_with(|| Instant::now() + every);
    }

    /// When terminals' history is next written.
    pub(super) fn history_due(&self) -> Option<Instant> {
        self.histories.due
    }

    /// Writes the history of the terminals that changed, if it's time to.
    pub(super) fn keep_histories_if_due(&mut self) {
        if self.histories.due.is_none_or(|due| due > Instant::now()) {
            return;
        }
        self.histories.due = None;
        // Nothing more while the workspaces are held for a shutdown: what
        // follows is everything ending.
        if self.held_until().is_some() {
            return;
        }
        self.keep_histories(false);
    }

    /// Has the thread write the history of every terminal that changed,
    /// putting off those whose output hasn't paused unless `now`.
    fn keep_histories(&mut self, now: bool) {
        let mut changed: Vec<(TerminalId, u8)> = self.histories.changed.drain().collect();
        // A few a turn, unless they're all wanted now: each copy is a
        // megabyte or so, and other work waits for the loop meanwhile.
        let later = if now {
            Vec::new()
        } else {
            changed.split_off(changed.len().min(MOST_A_TURN))
        };
        let mut put_off = false;
        for (id, times) in changed {
            if !self.keep_history(id, now || times >= MOST_PUT_OFF) {
                self.histories.changed.insert(id, times + 1);
                put_off = true;
            }
        }
        if !later.is_empty() {
            self.histories.changed.extend(later);
            self.histories.due = Some(Instant::now());
        } else if put_off
            && let Some(every) = self.config.history.as_ref().map(|config| config.every)
        {
            self.histories.due = Some(Instant::now() + every);
        }
    }

    /// Has the thread write terminal `id`'s history as it is now, or
    /// remove its file if none of it is kept. Says whether it's done, or
    /// put off as its output hasn't paused, unless `now`.
    fn keep_history(&mut self, id: TerminalId, now: bool) -> bool {
        if !self.history_kept_of(id) {
            // It may have had a file while it was kept.
            if self.histories.removed.insert(id) {
                let key = history::key(self.id, id.0);
                if let Some(writer) = self.history_writer() {
                    writer.send(key, Job::Remove);
                }
            }
            return true;
        }
        self.histories.removed.remove(&id);
        let Some(lines) = self.history_config().map(|config| config.lines) else {
            return true;
        };
        let Some(Hosted::Pty(terminal)) =
            self.terminals.get_mut(&id).map(|slot| &mut slot.terminal)
        else {
            return true;
        };
        if terminal.emulator().scrollback_needs_compaction() {
            if !now {
                return false;
            }
            terminal.compact_scrollback();
        }
        let keeping = match self.histories.marks.get_mut(&id) {
            Some(marks) => {
                marks.follow(terminal.emulator());
                marks.keeping()
            }
            None => Keeping::default(),
        };
        let key = history::key(self.id, id.0);
        // None kept writes nothing, and removes the file.
        let job = Job::Write {
            copy: Box::new(terminal.emulator().copy_to_keep(lines)),
            keeping,
        };
        if let Some(writer) = self.history_writer() {
            writer.send(key, job);
        }
        true
    }

    /// Has the history of every terminal that changed written now, as
    /// the daemon stops or upgrades: nothing after is. Waited for with
    /// [`histories_kept_last`](Self::histories_kept_last).
    pub(super) fn keep_histories_last(&mut self) {
        self.histories.due = None;
        if self.held_until().is_none() {
            self.keep_histories(true);
        }
    }

    /// Waits until `until` for what [`keep_histories_last`] had written
    /// to be written.
    ///
    /// [`keep_histories_last`]: Self::keep_histories_last
    pub(super) fn histories_kept_last(&mut self, until: Instant) {
        if let Some(writer) = self.histories.writer.take()
            && !writer.stop(until.saturating_duration_since(Instant::now()))
        {
            log::warn!("terminals' history wasn't all written as the daemon stopped");
        }
    }

    /// Has the history of every terminal that changed written now, as
    /// the machine is about to sleep or shut down, with `lock`, a copy of
    /// logind's, let go once it's on disk.
    pub(super) fn keep_histories_now(&mut self, lock: Option<std::os::fd::OwnedFd>) {
        self.histories.due = None;
        self.keep_histories(true);
        if let Some(lock) = lock
            && let Some(writer) = self.histories.writer.as_ref()
        {
            writer.let_go(lock);
        }
    }

    /// Lets terminal `id`'s history go, as it has closed for good. Not
    /// while the workspaces are held for a shutdown, which keep it.
    pub(super) fn history_closed(&mut self, id: TerminalId) {
        self.histories.changed.remove(&id);
        self.histories.marks.remove(&id);
        self.histories.removed.remove(&id);
        if self.held_until().is_some() || !self.history_kept_of_any() {
            return;
        }
        let key = history::key(self.id, id.0);
        if let Some(writer) = self.history_writer() {
            writer.send(key, Job::Remove);
        }
    }

    /// Whether this daemon keeps any terminal's history.
    fn history_kept_of_any(&self) -> bool {
        self.history_config().is_some()
    }

    /// Answers [`Request::KeepHistory`](clackatoo_protocol::Request::KeepHistory)
    /// from the client at `token`.
    pub(super) fn serve_keep_history(
        &mut self,
        token: Token,
        pane: PaneId,
        how: HistoryKeeping,
    ) -> Result<Response, Error> {
        // What's kept on disk is for the user to arrange, not a phone that
        // can type into terminals.
        if self.is_web_device(token) {
            return Err(Error::Other(
                "a device on the web app can't change what's kept of a terminal".into(),
            ));
        }
        let terminal = self.terminal_of_pane(pane).ok_or(Error::NoSuchPane(pane))?;
        // Forgetting, and keeping none, always work: its file, if it has
        // one, goes.
        let lets_go = matches!(how, HistoryKeeping::Forget | HistoryKeeping::Off);
        if !lets_go && !self.history_kept_of(terminal) {
            return Err(Error::Other(format!(
                "pane {pane}'s history is never kept: it's an agent's"
            )));
        }
        if self.config.history.is_none() {
            return Err(Error::Other(
                "terminals' history isn't kept: CLACKATOO_HISTORY is off".into(),
            ));
        }
        let Some(Hosted::Pty(pty)) =
            (self.terminals.get_mut(&terminal)).map(|slot| &mut slot.terminal)
        else {
            return Err(Error::IsAChat(terminal));
        };
        let marks = self.histories.marks.entry(terminal).or_default();
        marks.follow(pty.emulator());
        let said = match how {
            HistoryKeeping::On => {
                marks.off = false;
                format!("pane {pane}'s history is kept across a restart")
            }
            HistoryKeeping::Off => {
                marks.off = true;
                format!("pane {pane}'s history isn't kept across a restart")
            }
            HistoryKeeping::Forget => {
                // What it shows now is forgotten too: only what comes after,
                // from the cursor's line on, is kept. Behind a full-screen
                // program, the main screen's cursor isn't where it shows,
                // so all of that screen is.
                let emulator = pty.emulator();
                let primary = emulator.primary();
                let rows = if emulator.is_alternate_screen() {
                    primary.height()
                } else {
                    emulator.cursor().line
                };
                let line = primary.scrollback_end() + rows as u64;
                marks.from = marks.from.max(line);
                marks.numbered = numbering(emulator);
                format!("pane {pane}'s history kept so far is forgotten")
            }
            HistoryKeeping::BlockAt(line) => {
                let block = pty.emulator().block(line, Which::At).ok_or_else(|| {
                    Error::Other(format!("pane {pane} has no command there to keep"))
                })?;
                // Marked on its rows, which carry it wherever its text goes.
                pty.keep_lines(block.first..block.last + 1);
                let shown = block
                    .command
                    .map_or_else(|| "its output".to_owned(), |command| format!("`{command}`"));
                if marks.off {
                    format!("{shown} is kept across a restart")
                } else {
                    format!("{shown} is kept, as all of pane {pane}'s history is")
                }
            }
        };
        if *marks == Marks::default() {
            self.histories.marks.remove(&terminal);
        }
        self.workspaces_changed();
        // Written, or removed, at once: forgetting can't wait. Said done
        // only once it's on its way to the disk; otherwise tried again in
        // a moment, and said so.
        if self.history_writer().is_some() && self.keep_history(terminal, true) {
            Ok(Response::Text(said))
        } else {
            self.histories.changed.insert(terminal, MOST_PUT_OFF);
            self.histories.due = Some(Instant::now() + RETRY);
            Err(Error::Other(format!(
                "pane {pane}'s history can't be written now: it's tried again in a moment"
            )))
        }
    }

    /// Whether terminal `id`'s history is marked not to be kept, for the
    /// saved workspaces.
    pub(super) fn history_off(&self, id: TerminalId) -> bool {
        self.histories.marks.get(&id).is_some_and(|marks| marks.off)
    }

    /// The name of terminal `id`'s history file, for the saved
    /// workspaces, if its history is kept.
    pub(super) fn history_key(&self, id: TerminalId) -> Option<String> {
        self.history_kept_of(id)
            .then(|| history::key(self.id, id.0))
            .filter(|_| self.history_kept_of_any())
    }

    /// Reads the history kept of the terminals in `state`, as the daemon
    /// starts afresh, for them to take as they're made again: several
    /// files at once, on threads of their own.
    pub(super) fn read_histories(&mut self, state: &saved::State) {
        let Some(dir) = self.history_config().map(|config| config.dir.clone()) else {
            return;
        };
        let mut keys = Vec::new();
        for workspace in &state.workspaces {
            for tab in workspace.tabs.iter().chain(&workspace.background) {
                kept_keys(&tab.layout, &mut keys);
            }
        }
        if keys.is_empty() {
            return;
        }
        if let Err(why) = history::readable_dir(&dir) {
            log::warn!("terminals' history kept isn't read: {why}");
            return;
        }
        let started = Instant::now();
        let terminal = self.config.terminal.clone();
        let divider = divider(now_seconds());
        let readers = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(MOST_READERS);
        let chunk = keys.len().div_ceil(readers);
        let read: Vec<(String, KeptHistory)> = std::thread::scope(|scope| {
            let threads: Vec<_> = keys
                .chunks(chunk)
                .map(|keys| {
                    let (dir, terminal, divider) = (&dir, &terminal, &divider);
                    scope.spawn(move || {
                        keys.iter()
                            .filter_map(|key| read_one(dir, key, terminal, divider))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            threads
                .into_iter()
                .flat_map(|thread| thread.join().unwrap_or_default())
                .collect()
        });
        log::info!(
            "the history of {} terminals read in {} ms",
            read.len(),
            started.elapsed().as_millis()
        );
        self.histories.restoring = read.into_iter().collect();
    }

    /// Puts back what was kept of terminal `id`'s history, from the file
    /// `key` names, as it's made again, and whether it was marked not to
    /// be kept.
    pub(super) fn restore_history(&mut self, id: TerminalId, key: Option<&str>, off: bool) {
        let kept = key.and_then(|key| self.histories.restoring.remove(key));
        let at = now_seconds();
        let mut end = None;
        if let Some(kept) = kept
            && let Some(Hosted::Pty(terminal)) =
                self.terminals.get_mut(&id).map(|slot| &mut slot.terminal)
        {
            end = terminal.take_kept_history(kept, at);
        }
        if off {
            // What came back of a terminal marked off is what the user kept
            // of it, its rows still marked so: it's kept again.
            self.histories.marks.insert(
                id,
                Marks {
                    off,
                    ..Marks::default()
                },
            );
        }
        // Written again at once, under its new name: the old file goes
        // once every terminal made again has been (see
        // `history_restored`).
        if end.is_some() || off {
            self.histories.changed.insert(id, MOST_PUT_OFF);
        }
    }

    /// Once every terminal has been made again: writes the history each
    /// took under its new name, before [`sweep_histories`] removes the
    /// files of the terminals before the restart.
    ///
    /// [`sweep_histories`]: Self::sweep_histories
    pub(super) fn history_restored(&mut self) {
        self.histories.restoring.clear();
        if self.history_config().is_some() {
            self.keep_histories(true);
        }
    }

    /// Removes every history file but those of the terminals this daemon
    /// has, as it starts, however it starts: those of a daemon before,
    /// whose terminals weren't made again, and any a crash left half
    /// written. After what's asked to be written before.
    pub fn sweep_histories(&mut self) {
        if self.history_config().is_none() {
            return;
        }
        let keep: HashSet<String> = (self.terminals.keys())
            .map(|&id| history::key(self.id, id.0))
            .collect();
        if let Some(writer) = self.history_writer() {
            writer.sweep(keep);
        }
    }

    /// What's kept of each terminal's history beyond its lines, for the
    /// next build: those with anything to say.
    pub(super) fn hand_over_history(
        &self,
        id: TerminalId,
        terminal: &clackatoo_term::Terminal,
    ) -> Option<handover::History> {
        let marks = self.histories.marks.get(&id).cloned().unwrap_or_default();
        let hints: Vec<(u64, u8)> = (terminal.hints().into_iter())
            .map(|(line, hints)| (line, hints.bits()))
            .collect();
        let restarted = terminal.primary().restarted();
        let history = handover::History {
            terminal: id.0,
            off: marks.off,
            from: marks.from,
            numbered: (marks.numbered.0, marks.numbered.1 as u64),
            hints,
            restarted,
        };
        (history
            != handover::History {
                terminal: id.0,
                ..handover::History::default()
            })
        .then_some(history)
    }

    /// Takes what's kept of the terminals' history beyond their lines
    /// from the build before.
    pub(super) fn take_histories(&mut self, histories: &[handover::History]) {
        for history in histories {
            let id = TerminalId(history.terminal);
            let Some(Hosted::Pty(terminal)) =
                self.terminals.get_mut(&id).map(|slot| &mut slot.terminal)
            else {
                continue;
            };
            let hints: Vec<(u64, Hints)> = (history.hints.iter())
                .map(|&(line, bits)| (line, Hints::from_bits_truncate(bits)))
                .collect();
            terminal.restore_history_marks(&hints, history.restarted);
            // The next build may number lines afresh, which `follow` sees.
            let marks = Marks {
                off: history.off,
                from: history.from,
                numbered: (
                    history.numbered.0,
                    usize::try_from(history.numbered.1).unwrap_or(usize::MAX),
                ),
            };
            if marks != Marks::default() {
                self.histories.marks.insert(id, marks);
            }
        }
    }
}

/// The names of the history files of the terminals in `layout`, as kept.
fn kept_keys(layout: &saved::Layout, into: &mut Vec<String>) {
    match layout {
        saved::Layout::Pane(saved::Pane::Terminal {
            history: Some(key),
            elsewhere: None,
            ..
        }) if history::is_key(key) => into.push(key.clone()),
        saved::Layout::Pane(_) => {}
        saved::Layout::Split { parts, .. } => {
            for part in parts {
                kept_keys(part, into);
            }
        }
    }
}

/// The history the file `key` names, in `dir`, kept, made history for a
/// terminal made with `config`, ending in `divider`: `None` if there's
/// none, or it can't be read, which is said in the log.
fn read_one(
    dir: &Path,
    key: &str,
    config: &clackatoo_term::Config,
    divider: &str,
) -> Option<(String, KeptHistory)> {
    let lines = match history::read(dir, key) {
        Ok(Some(lines)) if !lines.is_empty() => lines,
        Ok(_) => return None,
        Err(err) => {
            log::warn!("a terminal's history kept couldn't be read ({key}): {err}");
            return None;
        }
    };
    match lines.history(config, divider) {
        Ok(history) => Some((key.to_owned(), history)),
        Err(err) => {
            log::warn!("a terminal's history kept couldn't be read ({key}): {err:?}");
            None
        }
    }
}

#[cfg(test)]
pub(in crate::server) mod tests {
    use std::time::Duration;

    use clackatoo_protocol::{NewTerminal, Size};

    use super::*;

    /// A daemon keeping terminals' history in `dir`, if `keeping`, with a
    /// terminal running `cat` in a workspace, and its pane.
    pub(in crate::server) fn keeping(dir: &Path, keeping: bool) -> (Daemon, TerminalId, PaneId) {
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir);
        let mut config = daemon.config.clone();
        config.workspaces = keeping.then(|| dir.join("state/workspaces"));
        config.history = Some(crate::HistoryConfig::in_dir(dir.join("state/history")));
        daemon.keeping = super::super::restore::Keeping::new(&config);
        daemon.config = config;
        let request = NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["cat".into()]),
            cwd: None,
            record_to: None,
        };
        let terminal = daemon.new_terminal(&request).unwrap();
        let pane = daemon.workspaces.new_workspace("kept".to_owned(), terminal);
        (daemon, terminal, pane)
    }

    /// Every start removes the files of terminals it doesn't have, and
    /// any a crash left half written, whether workspaces were made again
    /// or not, and keeps its own terminals'.
    #[test]
    fn a_start_sweeps_away_the_files_of_terminals_it_doesnt_have() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, _) = keeping(dir.path(), true);
        let history = dir.path().join("state/history");
        crate::recovery::make_dir(&history).unwrap();
        let own = history.join(format!("{}.history", history::key(daemon.id, terminal.0)));
        let old = history.join(format!("{}.history", history::key(daemon.id ^ 1, 3)));
        let half = history.join(format!(".{}.tmp", history::key(daemon.id ^ 1, 4)));
        let other = history.join("notes.txt");
        for path in [&own, &old, &half, &other] {
            std::fs::write(path, "").unwrap();
        }
        daemon.sweep_histories();
        daemon.keep_last();
        daemon.kept_last();
        assert!(own.exists());
        assert!(!old.exists() && !half.exists());
        assert!(other.exists(), "nothing but history files is touched");
    }

    /// Only an agent's own terminals are never kept: one the user's agent
    /// typed into is still the user's. One that becomes an agent's has
    /// its file removed, and forgetting works on it all the same.
    #[test]
    fn an_agents_terminal_isnt_kept_and_one_that_becomes_one_loses_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, pane) = keeping(dir.path(), true);
        let file = dir.path().join(format!(
            "state/history/{}.history",
            history::key(daemon.id, terminal.0)
        ));
        let chat = TerminalId(9999);
        if let Some(Hosted::Pty(pty)) = daemon
            .terminals
            .get_mut(&terminal)
            .map(|slot| &mut slot.terminal)
        {
            pty.emulator_mut().advance(b"printed\r\n");
        }
        let written = |daemon: &mut Daemon| {
            daemon.history_moved_on(terminal);
            daemon.keep_histories(true);
            daemon.histories_kept_last(Instant::now() + Duration::from_secs(10));
            file.exists()
        };
        daemon.commanded_by(terminal, chat, false);
        assert!(
            written(&mut daemon),
            "typed into by an agent, it's still the user's"
        );
        daemon.commanded_by(terminal, chat, true);
        assert!(!written(&mut daemon), "an agent's now, its file goes");
        let asked = daemon.serve_keep_history(Token(usize::MAX), pane, HistoryKeeping::Forget);
        assert!(matches!(asked, Ok(Response::Text(_))), "{asked:?}");
        let asked = daemon.serve_keep_history(Token(usize::MAX), pane, HistoryKeeping::On);
        assert!(asked.is_err(), "{asked:?}");
    }

    /// The lines terminal `terminal`'s history file holds, once what was
    /// asked is written.
    fn written(daemon: &mut Daemon, terminal: TerminalId) -> Vec<String> {
        daemon.history_moved_on(terminal);
        daemon.keep_histories(true);
        daemon.histories_kept_last(Instant::now() + Duration::from_secs(10));
        let dir = daemon.config.history.as_ref().unwrap().dir.clone();
        let key = history::key(daemon.id, terminal.0);
        (history::read(&dir, &key).unwrap())
            .map(|lines| lines.texts().into_iter().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// Prints `output` in terminal `terminal`, as its program would.
    fn print(daemon: &mut Daemon, terminal: TerminalId, output: &str) {
        if let Some(Hosted::Pty(pty)) =
            (daemon.terminals.get_mut(&terminal)).map(|slot| &mut slot.terminal)
        {
            pty.emulator_mut().advance(output.as_bytes());
        }
    }

    /// A block kept in a terminal marked off is marked on its own rows: a
    /// reset, which numbers lines afresh, keeps nothing that came after
    /// where it was.
    #[test]
    fn a_block_kept_in_a_terminal_marked_off_keeps_nothing_after_a_reset() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, pane) = keeping(dir.path(), true);
        let any = Token(usize::MAX);
        assert!((daemon.serve_keep_history(any, pane, HistoryKeeping::Off)).is_ok());
        let prompt = "\x1b]133;A\x07$ \x1b]133;B\x07";
        let ran = |command: &str, output: &str| {
            format!("{prompt}{command}\r\n\x1b]133;C\x07{output}\r\n\x1b]133;D;0\x07")
        };
        print(&mut daemon, terminal, &ran("echo kept", "kept"));
        print(&mut daemon, terminal, &ran("echo dropped", "dropped"));
        print(&mut daemon, terminal, prompt);
        let asked = daemon.serve_keep_history(any, pane, HistoryKeeping::BlockAt(1));
        assert!(asked.is_ok(), "{asked:?}");
        let kept = written(&mut daemon, terminal);
        assert_eq!(kept, ["$ echo kept", "kept"]);
        // Reset, lines numbered afresh: where the block was is new text.
        print(&mut daemon, terminal, "\x1bcnew one\r\nnew two\r\n");
        assert_eq!(written(&mut daemon, terminal), Vec::<String>::new());
    }

    /// Lines forgotten stay forgotten as the terminal is narrowed and
    /// widened, which numbers its lines again.
    #[test]
    fn lines_forgotten_stay_forgotten_through_a_resize() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, pane) = keeping(dir.path(), true);
        for n in 0..14 {
            print(
                &mut daemon,
                terminal,
                &format!("secret {n} {}\r\n", "x".repeat(30)),
            );
        }
        let asked = daemon.serve_keep_history(Token(usize::MAX), pane, HistoryKeeping::Forget);
        assert!(asked.is_ok(), "{asked:?}");
        print(&mut daemon, terminal, "after\r\n");
        assert!(
            written(&mut daemon, terminal)
                .iter()
                .all(|line| !line.contains("secret"))
        );
        for cols in [12, 40] {
            if let Some(Hosted::Pty(pty)) =
                (daemon.terminals.get_mut(&terminal)).map(|slot| &mut slot.terminal)
            {
                pty.emulator_mut().resize(Size::new(cols, 10).unwrap());
            }
            print(&mut daemon, terminal, "later\r\n");
            let kept = written(&mut daemon, terminal);
            assert!(
                kept.iter().all(|line| !line.contains("secret")),
                "{cols}: {kept:?}"
            );
        }
    }

    /// Forgetting while a full-screen program shows forgets everything on
    /// the main screen behind it, wherever the program's cursor is.
    #[test]
    fn forgetting_behind_a_full_screen_program_forgets_the_whole_main_screen() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, pane) = keeping(dir.path(), true);
        print(
            &mut daemon,
            terminal,
            "secret one\r\nsecret two\r\nsecret three\r\n$ ",
        );
        // The program's cursor at its top left.
        print(&mut daemon, terminal, "\x1b[?1049h\x1b[H");
        let asked = daemon.serve_keep_history(Token(usize::MAX), pane, HistoryKeeping::Forget);
        assert!(asked.is_ok(), "{asked:?}");
        let kept = written(&mut daemon, terminal);
        assert!(!kept.iter().any(|line| line.contains("secret")), "{kept:?}");
    }

    /// The user is told what they asked of a terminal's history is done
    /// only once it's on its way to the disk; otherwise it's tried again.
    #[test]
    fn forgetting_is_said_done_only_once_it_is_on_its_way() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _, pane) = keeping(dir.path(), true);
        let asked = daemon.serve_keep_history(Token(usize::MAX), pane, HistoryKeeping::Forget);
        assert!(matches!(asked, Ok(Response::Text(_))), "{asked:?}");
        assert!(daemon.histories.writer.is_some());

        // Not keeping them, as when another daemon does, nothing can be
        // written.
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, pane) = keeping(dir.path(), false);
        let asked = daemon.serve_keep_history(Token(usize::MAX), pane, HistoryKeeping::Off);
        assert!(
            matches!(&asked, Err(Error::Other(why)) if why.contains("tried again")),
            "{asked:?}"
        );
        assert!(daemon.histories.changed.contains_key(&terminal));
        let soon = Instant::now() + Duration::from_secs(1);
        assert!(daemon.histories.due.is_some_and(|due| due <= soon));
    }
}
