//! `incsearch` in a terminal's normal mode: while `/` or `?` is typed, the
//! cursor goes to the match the search would go to from where it started,
//! the view follows it, and its matches are lit (see
//! [`crate::editor`]'s, which this follows). Esc puts them back; Enter
//! searches from where it started, keeping the view.
//!
//! The daemon searches, as it holds all of history, so what's typed is
//! asked of it as the window settles before it draws, not at each key, and
//! only once the answer to the last is in: never more than one in flight,
//! however fast keys come, and an answer to what's no longer typed is
//! dropped, the latest asked instead. Nothing waits for these answers, as
//! keys wait for a search's (see [`Pane::waits_for_answer`]).

use clackatoo_protocol::{Match, Request};

use super::{Pane, Pending, normal};
use crate::motion::Cursor;

/// Where a search being typed started, to put back on Esc, and what was
/// last asked of it.
#[derive(Debug)]
pub(super) struct Start {
    cursor: Option<Cursor>,
    scroll: u64,
    found: Option<Match>,
    matching: Option<(Vec<char>, bool)>,
    /// What was typed when the daemon was last asked where it goes.
    asked: Option<String>,
}

impl Pane {
    /// A search starts being typed: where it starts is kept, with
    /// `incsearch`, to show it from and put back.
    pub(super) fn start_search_preview(&mut self) {
        self.search_start = self.incsearch.then(|| Start {
            cursor: self.cursor,
            scroll: self.scroll,
            found: self.found,
            matching: self.matching.clone(),
            asked: None,
        });
    }

    /// Asks the daemon where the search being typed would go from where it
    /// started, if what's typed has changed since it was last asked and
    /// it's not still answering, and lights its matches meanwhile. A search
    /// no longer being typed, by a way that didn't put things back, is put
    /// back now. Returns whether the pane must be redrawn.
    pub(crate) fn preview_search(&mut self) -> bool {
        let Some(start) = &self.search_start else {
            return false;
        };
        let Some(prompt) = &self.prompt else {
            self.end_search_preview(false);
            return true;
        };
        let typed = prompt.line.text();
        if start.asked.as_deref() == Some(typed) || self.previewing() {
            return false;
        }
        let (typed, backward) = (typed.to_owned(), prompt.backward);
        self.put_back_search_start(false);
        let Some(start) = &mut self.search_start else {
            return true;
        };
        start.asked = Some(typed.clone());
        let Some(from) = start.cursor.filter(|_| !typed.is_empty()) else {
            return true;
        };
        let exact = self.case.exact(&typed);
        self.found = None;
        self.matching = Some((typed.chars().collect(), exact));
        self.request(
            Request::Search {
                terminal: self.terminal(),
                exact,
                query: typed.clone(),
                from: normal::position(from.at),
                backward,
            },
            Pending::Preview(typed),
        );
        true
    }

    /// Whether the daemon is still answering where a search being typed
    /// would go.
    fn previewing(&self) -> bool {
        (self.requests.pending()).any(|pending| matches!(pending, Pending::Preview(_)))
    }

    /// Takes in where the daemon found `query`, asked of a search being
    /// typed: shown, if it's still what's typed, and else dropped.
    pub(super) fn previewed(&mut self, query: &str, found: Option<Match>) {
        let typed = self.prompt.as_ref().map(|prompt| prompt.line.text());
        let asked = (self.search_start.as_ref()).and_then(|start| start.asked.as_deref());
        if typed != Some(query) || asked != Some(query) {
            return;
        }
        if let Some(found) = found {
            self.found = Some(found);
            self.cursor = Some(Cursor::new(crate::motion::Point {
                line: found.start.line,
                col: found.start.col,
            }));
            self.follow_cursor();
        }
    }

    /// Forgets where the search being typed started, as history is
    /// numbered afresh: it's shown no more, and stays where it was shown.
    pub(super) fn forget_search_start(&mut self) {
        self.search_start = None;
    }

    /// Keeps the view the search being typed started with on the same
    /// lines, as `lines` more have scrolled into history.
    pub(super) fn search_start_scrolled(&mut self, lines: u64) {
        if let Some(start) = &mut self.search_start {
            start.scroll += lines;
        }
    }

    /// Stops showing the search being typed: the cursor goes back to where
    /// it started, and what was lit; and the view, unless `keep_view`, as
    /// Enter keeps it.
    pub(super) fn end_search_preview(&mut self, keep_view: bool) {
        self.put_back_search_start(keep_view);
        self.search_start = None;
    }

    /// Puts the cursor back where the search being typed started, and
    /// what was lit, and the view unless `keep_view`.
    fn put_back_search_start(&mut self, keep_view: bool) {
        let Some(start) = &self.search_start else {
            return;
        };
        self.cursor = start.cursor;
        self.found = start.found;
        self.matching = start.matching.clone();
        if !keep_view {
            self.scroll = start.scroll.min(self.max_scroll());
            self.fetch_history();
        }
    }
}
