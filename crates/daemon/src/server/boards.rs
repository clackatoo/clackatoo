//! Boards in the event loop: reading them when the daemon starts, making
//! each change and having it written, and sending them to the clients
//! that show them.
//!
//! The boards are read and written on a thread (see
//! [`crate::boards::disk`]), started as the daemon is. Until it has read
//! them, which takes a moment, nothing can be done to a board: agents'
//! calls wait for it (see `tools/boards.rs`), and clients that attach are
//! sent the board once it's there. Every change is made in memory first,
//! sent to the clients showing the board, and then written, so nothing
//! waits for the disk.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use clackatoo_protocol::{
    BoardAction, BoardAuthor, BoardId, BoardScope, BoardSummary, BoardUpdate, Error, Event,
    OpenBoard, PaneId, Response,
};
use mio::{Token, Waker};

use super::Daemon;
use crate::boards::disk::{Disk, Reading, Record};
use crate::boards::{self, Board, Boards};

/// The longest an upgrade, or the daemon stopping, waits for the boards'
/// last changes to be written.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// The boards, and what's needed to keep and show them.
#[derive(Debug)]
pub(super) struct BoardState {
    status: Status,
    pub(super) list: Boards,
    /// The thread reading and writing them, while it runs.
    disk: Option<Disk>,
    /// Where the thread puts what it read.
    reading: Reading,
    /// The clients showing each board.
    viewers: BTreeMap<BoardId, Vec<Token>>,
    /// Clients that attached to boards before they were read, to send
    /// them once they are.
    early: Vec<(Token, BoardId)>,
    /// Why the last change couldn't be written, if one couldn't.
    pub(super) failing: Option<String>,
    /// Damaged board files, moved aside when the boards were read.
    pub(super) quarantined: Vec<PathBuf>,
}

/// Whether the boards can be used.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// They're being read.
    Reading,
    Ready,
    /// They can't be used, as this says.
    Off(String),
}

impl BoardState {
    /// Starts reading the boards in `dir`, waking the loop with `waker`
    /// once they're read. With no `dir`, there are none.
    pub(super) fn start(dir: Option<PathBuf>, waker: &Arc<Waker>) -> Self {
        let reading = Reading::default();
        let mut state = Self {
            status: Status::Reading,
            list: Boards::default(),
            disk: None,
            reading: Arc::clone(&reading),
            viewers: BTreeMap::new(),
            early: Vec::new(),
            failing: None,
            quarantined: Vec::new(),
        };
        let Some(dir) = dir else {
            state.status = Status::Off("this daemon keeps no boards".to_owned());
            return state;
        };
        let waker = Arc::clone(waker);
        let wake = move || {
            if let Err(err) = waker.wake() {
                log::warn!("waking the event loop for the boards failed: {err}");
            }
        };
        match Disk::start(dir, reading, wake) {
            Ok(disk) => state.disk = Some(disk),
            Err(err) => state.status = Status::Off(format!("the boards couldn't be read: {err}")),
        }
        state
    }

    /// Whether they're still being read.
    pub(super) fn reading(&self) -> bool {
        self.status == Status::Reading
    }

    /// Checks they can be used, or says why not.
    pub(super) fn ready(&self) -> Result<(), String> {
        match &self.status {
            Status::Ready => Ok(()),
            Status::Reading => Err("the boards are still being read: try again".to_owned()),
            Status::Off(why) => Err(why.clone()),
        }
    }
}

impl Daemon {
    // --- Reading and writing ------------------------------------------------

    /// Takes the boards, once the thread has read them, and says what it
    /// couldn't write. Returns whether they've just been read, or found
    /// unreadable.
    pub(super) fn take_boards(&mut self) -> bool {
        if let Some(disk) = &self.boards.disk {
            let failed =
                std::mem::take(&mut *disk.failed.lock().unwrap_or_else(PoisonError::into_inner));
            if let Some(last) = failed.into_iter().last() {
                self.boards.failing = Some(last);
            }
        }
        if !self.boards.reading() {
            return false;
        }
        let Some(read) = self
            .boards
            .reading
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return false;
        };
        match read {
            Ok(loaded) => {
                log::info!("{} boards read", loaded.boards.list.len());
                self.boards.list = loaded.boards;
                self.boards.quarantined = loaded.quarantined;
                self.boards.status = Status::Ready;
            }
            Err(why) => {
                self.boards.status = Status::Off(why);
                // The thread has stopped: nothing is written.
                self.boards.disk = None;
            }
        }
        for (token, board) in std::mem::take(&mut self.boards.early) {
            if let Err(err) = self.attach_board(token, board) {
                self.tell_board(token, board, BoardUpdate::Gone(err.to_string()));
            }
        }
        true
    }

    /// Writes what's waiting, and stops the thread, for an upgrade or as
    /// the daemon stops, waiting a moment for it.
    pub(super) fn stop_boards(&mut self) {
        if let Some(disk) = self.boards.disk.take()
            && !disk.stop(STOP_WAIT)
        {
            log::error!("the boards' last changes may not have been written");
        }
    }

    /// Before an upgrade: waits up to `wait` for every change to the
    /// boards to be written and synced, then for the thread to stop,
    /// which lets go of their lock, for the next build to take. Fails,
    /// for the upgrade not to go ahead, if they aren't in time, or a
    /// change couldn't be written: the next build would read the boards
    /// without it. Not in time, the boards carry on as they were; if the
    /// thread wouldn't stop, there are none until the daemon restarts.
    pub(super) fn prepare_boards_for_upgrade(&mut self, wait: Duration) -> Result<(), String> {
        let Some(disk) = &self.boards.disk else {
            return Ok(());
        };
        disk.flush(wait)?;
        let disk = self.boards.disk.take().expect("just used");
        if !disk.stop(wait) {
            let why = "the boards' thread stopped answering: restart the daemon".to_owned();
            self.boards.status = Status::Off(why.clone());
            return Err(why);
        }
        Ok(())
    }

    /// Makes a board called `name`, for `purpose`, seen from `scope`, by
    /// `by`.
    pub(super) fn create_board(
        &mut self,
        name: &str,
        purpose: &str,
        scope: BoardScope,
        by: &BoardAuthor,
    ) -> Result<BoardId, String> {
        self.boards.ready()?;
        let header = self
            .boards
            .list
            .create(name, purpose, scope, by, boards::now())?;
        let id = self.boards.list.add(&header);
        if let Some(disk) = &self.boards.disk {
            disk.create(&header);
        }
        log::info!("board {id} made: {:?}", header.name);
        Ok(id)
    }

    /// Changes board `id` by the record `make` makes of it, if it makes
    /// one: applies it, has it written, and tells the clients showing the
    /// board. Returns the entry it added or changed, if it's about one.
    pub(super) fn change_board(
        &mut self,
        id: BoardId,
        make: impl FnOnce(&Board) -> Result<Record, String>,
    ) -> Result<Option<u32>, String> {
        self.boards.ready()?;
        let board =
            (self.boards.list.get_mut(id)).ok_or_else(|| format!("there's no board {id}"))?;
        let record = make(board)?;
        let line = crate::boards::disk::line_of(&record);
        board.fits(&record, &line)?;
        let entry = board.apply(&record)?;
        board.bytes += line.len() as u64;
        if let Some(disk) = &self.boards.disk {
            disk.append(id.0, line);
        }
        let update = BoardUpdate::Changed {
            summary: board.summary.clone(),
            entries: (entry.and_then(|entry| board.entry(entry)).cloned())
                .into_iter()
                .collect(),
        };
        self.tell_board_viewers(id, &update);
        Ok(entry)
    }

    // --- Panes ----------------------------------------------------------

    /// The name of the workspace `pane` is in.
    pub(super) fn workspace_of(&self, pane: PaneId) -> Option<String> {
        let workspaces = self.workspaces.get();
        let (workspace, _, _) = workspaces.find_pane(pane)?;
        Some(workspace.name.clone())
    }

    /// The board a new pane beside `beside` shows: the one `open` names,
    /// as the workspace `beside` is in sees it, made there if it's named
    /// and there's none.
    pub(super) fn open_board(
        &mut self,
        open: OpenBoard,
        beside: Option<PaneId>,
    ) -> Result<BoardId, Error> {
        self.boards.ready().map_err(Error::Boards)?;
        let workspace = beside.and_then(|pane| self.workspace_of(pane));
        match open {
            OpenBoard::Id(id) => {
                let board = self.boards.list.get(id).ok_or(Error::NoSuchBoard(id))?;
                if beside.is_some() && !board.seen_from(workspace.as_deref()) {
                    return Err(Error::Boards(format!(
                        "board {id} belongs to another workspace"
                    )));
                }
                Ok(id)
            }
            OpenBoard::Named(name) => {
                if let Some(id) = self.boards.list.find(&name, workspace.as_deref()) {
                    return Ok(id);
                }
                let scope = workspace.map_or(BoardScope::Global, BoardScope::Workspace);
                self.create_board(&name, "", scope, &BoardAuthor::User)
                    .map_err(Error::Boards)
            }
        }
    }

    /// Answers [`Request::AttachBoard`](clackatoo_protocol::Request::AttachBoard):
    /// sends board `id` to the client at `token`, then each change to it.
    /// Before the boards are read, it's sent once they are.
    pub(super) fn attach_board(&mut self, token: Token, id: BoardId) -> Result<Response, Error> {
        if self.boards.reading() {
            if !self.boards.early.contains(&(token, id)) {
                self.boards.early.push((token, id));
            }
            return Ok(Response::Done);
        }
        self.boards.ready().map_err(Error::Boards)?;
        let board = self.boards.list.get(id).ok_or(Error::NoSuchBoard(id))?;
        let update = BoardUpdate::Snapshot {
            summary: board.summary.clone(),
            entries: board.entries.clone(),
        };
        let viewers = self.boards.viewers.entry(id).or_default();
        if !viewers.contains(&token) {
            viewers.push(token);
        }
        self.tell_board(token, id, update);
        Ok(Response::Done)
    }

    /// Answers [`Request::DetachBoard`](clackatoo_protocol::Request::DetachBoard).
    pub(super) fn detach_board(&mut self, token: Token, id: BoardId) -> Response {
        if let Some(viewers) = self.boards.viewers.get_mut(&id) {
            viewers.retain(|&viewer| viewer != token);
        }
        self.boards.early.retain(|&early| early != (token, id));
        Response::Done
    }

    /// Forgets the client at `token`, which has gone.
    pub(super) fn forget_board_viewer(&mut self, token: Token) {
        for viewers in self.boards.viewers.values_mut() {
            viewers.retain(|&viewer| viewer != token);
        }
        self.boards.early.retain(|&(early, _)| early != token);
    }

    /// Closes `pane`, which shows a board. The board stays: it's kept on
    /// disk, for any pane or agent.
    pub(super) fn close_board_pane(&mut self, pane: PaneId) {
        if self.workspaces.remove_pane(pane) {
            self.workspaces_changed();
        }
    }

    /// Acts on what the client at `token` asked of board `id`, as the
    /// user, telling it why not if it can't be done.
    pub(super) fn serve_board(&mut self, token: Token, id: BoardId, action: BoardAction) {
        let user = BoardAuthor::User;
        let now = boards::now();
        let done = match action {
            BoardAction::Post { kind, text } => {
                self.change_board(id, |board| board.post(&user, now, kind, &text, Vec::new()))
            }
            BoardAction::Edit { entry, text } => {
                self.change_board(id, |board| board.edit(&user, now, entry, &text, true))
            }
            BoardAction::Release { entry } => {
                self.change_board(id, |board| board.release(&user, now, entry, true))
            }
            BoardAction::Archive(archived) => {
                self.change_board(id, |board| board.archive(&user, now, archived))
            }
        };
        if let Err(why) = done {
            self.tell_board(token, id, BoardUpdate::Refused(why));
        }
    }

    /// What the boards a workspace called `workspace`, if it's in one,
    /// sees are: the open ones first, each by name.
    pub(super) fn board_summaries(&self, workspace: Option<&str>) -> Vec<BoardSummary> {
        let mut seen: Vec<BoardSummary> = (self.boards.list.list.values())
            .filter(|board| board.seen_from(workspace))
            .map(|board| board.summary.clone())
            .collect();
        seen.sort_by(|a, b| {
            (a.archived, a.name.to_lowercase()).cmp(&(b.archived, b.name.to_lowercase()))
        });
        seen
    }

    /// Sends `update` about board `id` to the client at `token`.
    fn tell_board(&mut self, token: Token, board: BoardId, update: BoardUpdate) {
        self.send_event(token, Event::Board { board, update });
    }

    /// Sends `update` about board `id` to every client showing it.
    fn tell_board_viewers(&mut self, board: BoardId, update: &BoardUpdate) {
        let viewers = self.boards.viewers.get(&board).cloned().unwrap_or_default();
        for token in viewers {
            self.tell_board(token, board, update.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use clackatoo_protocol::PostKind;

    use super::*;
    use crate::boards::disk::{Disk, Reading};
    use crate::server::agent_files::tests::daemon_with_chat;

    /// A daemon keeping its boards in `boards`, once they're read.
    fn daemon_with_boards(dir: &std::path::Path, boards: &std::path::Path) -> Daemon {
        let (mut daemon, _) = daemon_with_chat(dir);
        daemon.boards = BoardState::start(Some(boards.to_owned()), &daemon.waker);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !daemon.take_boards() {
            assert!(Instant::now() < deadline, "the boards were never read");
            std::thread::sleep(Duration::from_millis(5));
        }
        daemon
    }

    #[test]
    fn an_upgrade_waits_for_the_boards_to_be_written_and_let_go_of() {
        let dir = tempfile::tempdir().unwrap();
        let boards = tempfile::tempdir().unwrap();
        let mut daemon = daemon_with_boards(dir.path(), boards.path());
        let id = daemon
            .create_board("plan", "", BoardScope::Global, &BoardAuthor::User)
            .unwrap();
        // Not written in time: no upgrade, and the boards carry on.
        let disk = daemon.boards.disk.as_ref().unwrap();
        disk.stall(Duration::from_millis(500));
        let refused = daemon
            .prepare_boards_for_upgrade(Duration::from_millis(50))
            .unwrap_err();
        assert!(refused.contains("weren't written within"), "{refused}");
        assert!(daemon.boards.disk.is_some());
        let user = BoardAuthor::User;
        let post = |board: &Board| board.post(&user, 1, PostKind::Note, "x", Vec::new());
        daemon.change_board(id, post).unwrap();
        // In time, everything is on disk, and the lock is let go of, for
        // the next build to take.
        daemon
            .prepare_boards_for_upgrade(Duration::from_secs(5))
            .unwrap();
        assert!(daemon.boards.disk.is_none());
        let reading = Reading::default();
        let next = Disk::start(boards.path().to_owned(), Arc::clone(&reading), || {}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let read = loop {
            if let Some(read) = reading.lock().unwrap().take() {
                break read;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        };
        let read = read.expect("the lock was let go of");
        assert_eq!(read.boards.get(id).unwrap().entries.len(), 1);
        assert!(next.stop(Duration::from_secs(5)));
    }
}
