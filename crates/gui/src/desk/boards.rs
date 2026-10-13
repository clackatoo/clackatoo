//! The desk's boards: panes showing a board (see [`crate::board`]),
//! attached as the workspace shown's buffers and explorers are, and the
//! commands that open them.
//!
//! `:Board NAME` opens the board called that, as the workspace shown sees
//! it, beside the focused pane, making it in the workspace if there's
//! none; `:Board` alone, or `:Boards`, picks one in the finder. Several
//! panes can show one board: it's attached once, and what the daemon says
//! of it goes to each.

use std::collections::{HashMap, HashSet};

use clackatoo_protocol::{
    BoardId, ClientMessage, Event, OpenBoard, PaneId, PaneTerminal, Request, Side, Size,
};

use super::{Asked, Desk};
use crate::board::{Ask, BoardPane};
use crate::command::Place;
use crate::input::KeyPress;

/// The board panes, and the boards attached.
#[derive(Debug, Default)]
pub(super) struct Boards {
    panes: HashMap<PaneId, BoardPane>,
    attached: HashSet<BoardId>,
}

impl Desk {
    /// The board `pane` shows, if it shows one.
    pub(crate) fn board(&self, pane: PaneId) -> Option<&BoardPane> {
        self.boards.panes.get(&pane)
    }

    /// What the bar says for the focused pane, if it's an explorer or a
    /// board, and whether it's being typed in.
    pub(super) fn panel_label(&self) -> Option<(String, bool)> {
        (self.focused_explorer())
            .map(|explorer| (explorer.label(), explorer.prompting()))
            .or_else(|| (self.focused_board()).map(|board| (board.label(), board.prompting())))
    }

    /// The focused pane's board, if it shows one.
    pub(super) fn focused_board(&self) -> Option<&BoardPane> {
        self.boards.panes.get(&self.shown?.pane)
    }

    /// Makes a board pane for every pane showing a board in the workspace
    /// shown, and attaches to their boards; forgets those of panes gone.
    pub(super) fn attach_shown_boards(&mut self) {
        let shown: Vec<(PaneId, BoardId)> = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace))
            .map(|workspace| {
                (workspace.tabs.iter())
                    .flat_map(|tab| tab.layout.panes())
                    .filter_map(|pane| Some((pane.id, pane.content.board()?)))
                    .collect()
            })
            .unwrap_or_default();
        let anywhere: HashSet<BoardId> = (self.workspaces.list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .filter_map(|pane| pane.content.board())
            .collect();
        let boards = &mut self.boards;
        boards
            .panes
            .retain(|pane, board| shown.contains(&(*pane, board.board())));
        for &(pane, board) in &shown {
            boards
                .panes
                .entry(pane)
                .or_insert_with(|| BoardPane::new(board, self.options.cursorline));
        }
        if !self.connected {
            return;
        }
        let wanted: HashSet<BoardId> = boards.panes.values().map(BoardPane::board).collect();
        let gone: Vec<BoardId> = boards.attached.difference(&wanted).copied().collect();
        let new: Vec<BoardId> = wanted.difference(&boards.attached).copied().collect();
        for board in gone {
            self.boards.attached.remove(&board);
            if anywhere.contains(&board) {
                self.request(Request::DetachBoard { board }, Asked::Quiet);
            }
        }
        for board in new {
            self.boards.attached.insert(board);
            self.request(Request::AttachBoard { board }, Asked::Quiet);
        }
    }

    /// Sizes the board pane `pane` to its tile.
    pub(super) fn fit_board(&mut self, pane: PaneId, size: Size) {
        if let Some(board) = self.boards.panes.get_mut(&pane) {
            board.resize(size);
        }
    }

    /// Passes the options on to every board pane, as they've changed.
    pub(super) fn board_options(&mut self) {
        for board in self.boards.panes.values_mut() {
            board.set_cursorline(self.options.cursorline);
        }
    }

    /// Lays out the rows of the board panes in the tab shown.
    pub(crate) fn settle_boards(&mut self) {
        let Some(tab) = self.shown_tab() else { return };
        let shown: Vec<PaneId> = tab.layout.panes().iter().map(|pane| pane.id).collect();
        for pane in shown {
            if let Some(board) = self.boards.panes.get_mut(&pane) {
                board.settle();
            }
        }
    }

    /// Forgets what was attached, as a new connection does.
    pub(super) fn boards_reconnected(&mut self) {
        self.boards.attached.clear();
    }

    /// Takes in what the daemon says about a board. Returns whether the
    /// window must be redrawn.
    pub(crate) fn board_event(&mut self, event: Event) -> bool {
        let Event::Board { board, update } = event else {
            return false;
        };
        let mut shown = false;
        let tab = self.shown_tab().map(|tab| tab.layout.clone());
        for (&pane, showing) in &mut self.boards.panes {
            if showing.board() == board {
                showing.update(update.clone());
                shown |= tab.as_ref().is_some_and(|tab| tab.contains(pane));
            }
        }
        shown
    }

    /// The board panes' messages for the daemon.
    pub(super) fn take_board_outbox(&mut self) -> Vec<ClientMessage> {
        let connected = self.connected;
        let mut messages = Vec::new();
        for pane in self.boards.panes.values_mut() {
            let board = pane.board();
            let actions = pane.take_outbox();
            if connected {
                (messages).extend(
                    (actions.into_iter()).map(|action| ClientMessage::Board { board, action }),
                );
            }
        }
        messages
    }

    /// Hands `press` to the focused board pane, then does what it asks.
    /// Returns whether the window must be redrawn.
    pub(super) fn board_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(pane) = self.shown.map(|shown| shown.pane) else {
            return false;
        };
        let Some(board) = self.boards.panes.get_mut(&pane) else {
            return false;
        };
        let mut redraw = false;
        for stroke in crate::editor::strokes(press, board.prompting()) {
            redraw |= board.stroke(stroke);
        }
        for ask in board.take_asks() {
            redraw = true;
            match ask {
                Ask::Close => self.request(Request::ClosePane { pane }, Asked::Command),
            }
        }
        redraw
    }

    /// Stops the macro being recorded, if `press` is `q` in the focused
    /// board pane and one is, rather than closing the pane. Returns
    /// whether it did.
    pub(super) fn stop_recording_in_board(&mut self, press: &KeyPress<'_>) -> bool {
        let quiet =
            (self.focused_board()).is_some_and(|board| !board.prompting() && !board.pending());
        let q = crate::keymap::Chord::of(press) == Some(crate::keymap::Chord::typed('q'));
        if quiet && q && self.recording().is_some() {
            self.shared_mut().registers.stop_recording();
            return true;
        }
        false
    }

    /// A click at `cell` of the board pane `pane`: the cursor goes to the
    /// entry there. Returns whether the window must be redrawn.
    pub(super) fn board_click(&mut self, pane: PaneId, cell: [i64; 2]) -> bool {
        let Ok(row) = usize::try_from(cell[0]) else {
            return false;
        };
        (self.boards.panes.get_mut(&pane)).is_some_and(|board| board.click(row))
    }

    /// Scrolls the board pane `pane` by `lines`, as the wheel does.
    pub(super) fn board_wheel(&mut self, pane: PaneId, lines: i64) -> bool {
        (self.boards.panes.get_mut(&pane)).is_some_and(|board| board.scroll(lines))
    }

    /// Whether `pane` is a board pane, which is always in normal mode.
    pub(super) fn is_board(&self, pane: PaneId) -> bool {
        self.boards.panes.contains_key(&pane)
    }

    /// `:Board NAME`: opens the board called `name`, as the workspace shown
    /// sees it, where `place` says, or beside the focused pane; with no
    /// name, picks one in the finder.
    pub(super) fn open_board(&mut self, name: Option<String>, place: Place) {
        match name {
            Some(name) => self.show_board(OpenBoard::Named(name), place),
            None => self.open_finder(crate::finder::Kind::Boards, String::new()),
        }
    }

    /// Shows the board `open` names where `place` says: beside the
    /// focused pane, to its right, unless told another side, or in a new
    /// tab.
    pub(super) fn show_board(&mut self, open: OpenBoard, place: Place) {
        let Some(shown) = self.shown else { return };
        let terminal = PaneTerminal::Board(open);
        let request = match place {
            Place::Tab => Request::NewTab {
                workspace: shown.workspace,
                terminal,
            },
            Place::Beside(side) => Request::Split {
                pane: shown.pane,
                side,
                terminal,
            },
            Place::Here => Request::Split {
                pane: shown.pane,
                side: Side::Right,
                terminal,
            },
        };
        self.request(request, Asked::Pane);
    }
}
