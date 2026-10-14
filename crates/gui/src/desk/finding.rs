//! The finder in the desk: opening it on the project the focused pane is
//! in, asking the daemon for what to list and preview, passing on what
//! comes, and opening what's picked: in place of the focused pane's file
//! as `:e` does, beside it, or in a new tab, with the cursor on the line
//! picked once the file's text has come.
//!
//! A grep, and a search of the project's symbols, waits for typing to
//! pause a moment before it searches, so a word typed quickly is one
//! search, not one a letter. The window wakes for it, to draw again, and
//! it's started as it's drawn.
//!
//! The symbols in the focused editor's file are asked of its language
//! server as other questions about it are, and the answer, in order with
//! the file's edits, fills the list once it comes.
//!
//! The preview is asked for as the finder is drawn, for what it shows
//! picked then, so a flood of files streaming in costs one sort a frame,
//! not one a batch.
//!
//! Each search the daemon makes for the finder has a number of the
//! window's own. What comes for a search no longer wanted, such as a grep
//! for what was typed before the last key, is dropped by its number, and
//! the daemon is told to stop it.

use std::time::{Duration, Instant};

use clackatoo_protocol::wire::SYMBOLS;
use clackatoo_protocol::{
    ClientMessage, FileLocation, Found, OpenFile, PaneTerminal, Query, Request, Search,
};

use super::{Asked, Desk};
use crate::command::Place;
use crate::finder::view::FinderView;
use crate::finder::{Finder, Kind, Opening, Outcome, PREVIEW_LINES};
use crate::input::KeyPress;
use crate::options::Case;

/// How long a grep, or a search of the project's symbols, waits for the
/// next key before it searches.
pub(super) const GREP_WAIT: Duration = Duration::from_millis(80);

impl Desk {
    /// Opens the finder for `kind`, starting with `query` typed, on the
    /// project the focused pane is in.
    pub(crate) fn open_finder(&mut self, kind: Kind, query: String) {
        self.close_finder();
        self.close_focused_panel();
        if !self.connected {
            self.refuse("the finder needs the daemon, which is out of reach".into());
            return;
        }
        let case = Case::of(&self.options);
        match kind {
            Kind::Marks => return self.pick_mark(),
            Kind::Lessons => return self.tutor(None),
            Kind::News => return self.tutor(Some("new")),
            Kind::Symbols if !self.needs_daemon("listing a file's symbols", SYMBOLS) => return,
            Kind::ProjectSymbols if !self.needs_daemon("finding symbols", SYMBOLS) => return,
            _ => {}
        }
        // Asked of the file's language server, about its text as it is.
        let asked = if kind == Kind::Symbols {
            let Some((_, id)) = self.ask_about_focused_file(Query::Symbols) else {
                self.refuse("this pane has no file to list the symbols of".into());
                return;
            };
            Some(id)
        } else {
            None
        };
        let mut finder = Finder::new(kind, query, case);
        if let Some(id) = asked {
            finder.ask(id);
        }
        self.finder = Some(finder);
        match kind {
            Kind::Files => self.search(Search::Files),
            Kind::Buffers => self.search(Search::Buffers),
            Kind::Grep | Kind::ProjectSymbols => self.grep(),
            Kind::Diagnostics => self.search(Search::Diagnostics),
            Kind::Boards => self.search(Search::Boards),
            // Their places are given them, or asked for above.
            Kind::Places | Kind::Marks | Kind::Lessons | Kind::News | Kind::Symbols => {}
        }
    }

    /// Opens the finder on `found`, the places an answer about the file at
    /// `here` gave.
    pub(super) fn pick_place(&mut self, found: &[FileLocation], here: &str) {
        self.close_finder();
        self.close_focused_panel();
        let case = Case::of(&self.options);
        self.finder = Some(Finder::places(Kind::Places, found, here, case));
    }

    /// Closes the panel by the focused editor's cursor, which the finder
    /// goes over.
    fn close_focused_panel(&mut self) {
        if let Some(shown) = self.shown {
            self.close_editor_panel(shown.pane);
        }
    }

    /// Whether the finder is open, or the start page, which takes keys
    /// as it does.
    pub(super) fn finding(&self) -> bool {
        self.finder.is_some() || self.start.is_some()
    }

    /// Closes the finder, stopping the searches it was waiting on, or the
    /// start page.
    pub(super) fn close_finder(&mut self) {
        self.start = None;
        self.grep_at = None;
        let Some(finder) = self.finder.take() else {
            return;
        };
        for id in finder.search().into_iter().chain(finder.preview_search()) {
            self.stop_search(id);
        }
    }

    /// Handles a key while the finder is open.
    pub(super) fn finder_key(&mut self, press: &KeyPress<'_>) {
        if self.start.is_some() {
            return self.start_key(press);
        }
        let Some(finder) = &mut self.finder else {
            return;
        };
        let outcome = finder.key(press);
        self.finder_did(outcome);
    }

    /// A click on row `row` of the finder's list, or a double click if
    /// `open`, which opens what it shows.
    pub(crate) fn finder_click(&mut self, row: usize, open: bool) {
        if let Some(page) = &mut self.start {
            let outcome = page.click(row, open);
            self.start_did(outcome);
        } else if let Some(finder) = &mut self.finder {
            let outcome = finder.click(row, open);
            self.finder_did(outcome);
        }
    }

    /// The mouse's wheel turned `lines` over the finder: up, for more than
    /// none, moves to things before.
    pub(crate) fn finder_wheel(&mut self, lines: i64) {
        let by = isize::try_from(-lines).unwrap_or_default();
        if let Some(page) = &mut self.start {
            let outcome = page.scroll(by);
            self.start_did(outcome);
        } else if let Some(finder) = &mut self.finder {
            let outcome = finder.scroll(by);
            self.finder_did(outcome);
        }
    }

    /// Types `text` into the finder.
    pub(super) fn paste_into_finder(&mut self, text: &str) {
        if let Some(page) = &mut self.start {
            let outcome = page.paste(text);
            self.start_did(outcome);
        } else if let Some(finder) = &mut self.finder {
            let outcome = finder.paste(text);
            self.finder_did(outcome);
        }
    }

    /// Carries out what a key did to the finder.
    fn finder_did(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Nothing | Outcome::Changed => {}
            // Nothing to look for empties the list at once.
            Outcome::Search if self.finder.as_ref().is_some_and(|f| f.query().is_empty()) => {
                self.grep_at = None;
                self.grep();
            }
            Outcome::Search => self.grep_at = Some(Instant::now() + GREP_WAIT),
            Outcome::Close => self.close_finder(),
            // Said, and a macro typing it stops, as at a key that fails.
            Outcome::NothingPicked => self.refuse("nothing in the finder is picked".into()),
            Outcome::Open(opening) => {
                let kind = self.finder.as_ref().map(Finder::kind);
                self.close_finder();
                if kind == Some(Kind::Boards) {
                    self.open_picked_board(&opening);
                } else if matches!(kind, Some(Kind::Lessons | Kind::News)) {
                    // A lesson, or what's new, goes to the lesson.
                    self.open_lesson(&opening.path);
                } else {
                    self.open_picked(opening);
                }
            }
        }
    }

    /// Greps for what's typed in the finder, instead of what was, unless
    /// nothing is; or for the project's symbols, searches for those whose
    /// names match it, whatever it is, as a language server lists some
    /// for nothing typed.
    fn grep(&mut self) {
        let Some(finder) = &mut self.finder else {
            return;
        };
        let before = finder.search();
        let pattern = finder.query().to_owned();
        let symbols = finder.kind() == Kind::ProjectSymbols;
        if pattern.is_empty() && !symbols {
            finder.not_searching();
        }
        if let Some(id) = before {
            self.stop_search(id);
        }
        if symbols {
            self.search(Search::Symbols { query: pattern });
        } else if !pattern.is_empty() {
            let exact = Case::of(&self.options).exact(&pattern);
            self.search(Search::Grep { pattern, exact });
        }
    }

    /// When the window must wake for the finder's grep to search.
    pub(crate) fn grep_at(&self) -> Option<Instant> {
        self.grep_at
    }

    /// Greps for what's typed now, without waiting for typing to pause, as
    /// for a macro, which waits for what it finds.
    pub(super) fn grep_now(&mut self) {
        self.grep_at = None;
        self.grep();
    }

    /// Greps for what's typed, if typing has paused long enough by `now`.
    pub(super) fn grep_if_due(&mut self, now: Instant) {
        if self.grep_at.is_some_and(|at| at <= now) {
            self.grep_at = None;
            self.grep();
        }
    }

    /// Asks the daemon for `search`, from the focused pane, to fill the
    /// finder's list.
    fn search(&mut self, search: Search) {
        let id = self.send_search(search);
        if let Some(finder) = &mut self.finder {
            finder.searching(id);
        }
    }

    /// Asks the daemon for a preview of the thing picked, if the finder
    /// hasn't got or asked for one.
    fn ask_preview(&mut self) {
        let Some(finder) = &mut self.finder else {
            return;
        };
        let Some((path, line)) = finder.preview_wanted() else {
            return;
        };
        let before = finder.preview_search();
        if let Some(id) = before {
            self.stop_search(id);
        }
        let id = self.send_search(Search::Preview {
            path: path.clone(),
            line,
            lines: PREVIEW_LINES,
        });
        if let Some(finder) = &mut self.finder {
            finder.previewing(id, path, line);
        }
    }

    /// Asks the daemon for `search`, from the focused pane. Returns the
    /// search's number.
    fn send_search(&mut self, search: Search) -> u64 {
        self.searches += 1;
        let id = self.searches;
        if self.connected {
            // From an editor's folder, the search is from the folder shown.
            let pane = self.shown.map(|shown| shown.pane);
            if let Some(pane) = pane {
                self.root_folder_first(pane);
            }
            (self.outbox).push(ClientMessage::Find { id, pane, search });
        }
        id
    }

    fn stop_search(&mut self, id: u64) {
        if self.connected {
            self.outbox.push(ClientMessage::StopFinding { id });
        }
    }

    /// Takes in what the daemon's search numbered `id` found. Returns
    /// whether the window must be redrawn.
    pub(crate) fn found(&mut self, id: u64, found: Found) -> bool {
        let Some(finder) = &mut self.finder else {
            return false;
        };
        if finder.search() == Some(id) {
            finder.found(found);
            // A macro waiting for what it finds goes on.
            self.type_macros();
        } else if finder.preview_search() == Some(id) {
            finder.preview_found(found);
        } else {
            return false;
        }
        self.settle_lesson();
        true
    }

    /// What the finder shows, if it's open, with `list` things of so many
    /// cells, and a preview of `preview` lines of so many, if there's room;
    /// asking for the preview of what's picked, if it hasn't been.
    pub(crate) fn finder_view(
        &mut self,
        list: (usize, usize),
        preview: Option<(usize, usize)>,
    ) -> Option<FinderView> {
        if let Some(page) = &mut self.start {
            return Some(page.view(list, preview));
        }
        self.grep_if_due(Instant::now());
        let view = self.finder.as_mut()?.view(list, preview);
        if preview.is_some() {
            self.ask_preview();
        }
        Some(view)
    }

    /// Opens the board picked in the finder, whose ID is the path it
    /// gave: beside the focused pane as Enter or Ctrl+V say, below for
    /// Ctrl+X, or in a new tab for Ctrl+T.
    fn open_picked_board(&mut self, opening: &Opening) {
        let Ok(id) = opening.path.parse() else { return };
        let place = match opening.place {
            Place::Here => Place::Beside(clackatoo_protocol::Side::Right),
            place => place,
        };
        self.show_board(
            clackatoo_protocol::OpenBoard::Id(clackatoo_protocol::BoardId(id)),
            place,
        );
    }

    /// Opens what was picked in the finder.
    fn open_picked(&mut self, opening: Opening) {
        let Some(shown) = self.shown else { return };
        let Opening { path, at, place } = opening;
        let landing = at.map(|(line, col)| FileLocation {
            path: path.clone(),
            line,
            col,
            offset: None,
            text: String::new(),
        });
        if place == Place::Here
            && let Some((_, copy)) = self.focused_editor()
        {
            // As `:e` does, so Ctrl+O comes back here. A place in the file
            // shown needs nothing opened.
            let elsewhere = copy.path() != path;
            self.record_jump(shown.pane, None);
            if elsewhere {
                self.open_here(shown.pane, path);
            }
            if let Some(landing) = landing {
                self.land_at(shown.pane, landing);
            }
            return;
        }
        let terminal = PaneTerminal::File(OpenFile { path, cwd: None });
        let request = match place {
            Place::Tab | Place::Here => Request::NewTab {
                workspace: shown.workspace,
                terminal,
            },
            Place::Beside(side) => Request::Split {
                pane: shown.pane,
                side,
                terminal,
            },
        };
        self.request(request, Asked::PaneAt(landing));
    }
}
