//! `/` `?` `n` `N` in a terminal's normal mode, on the search every pane
//! shares (see [`crate::vim::search`]), with `incsearch`'s preview and
//! `hlsearch`'s lit matches.
//!
//! The daemon searches, as it holds all of history, so finding is asked
//! of it and answered later. A count, as `3n`, is asked a match at a time,
//! each from the last, until it's back at the first, when only as many as
//! are left of a time round are asked. Keys typed meanwhile wait for the
//! answer (see [`Pane::waits_for_answer`]).
//!
//! What's typed is asked once the window has handled what came in, just
//! before it sends what it has (see [`crate::desk::Desk::preview_searches`]),
//! not at each key, and only once the answer to the last is in: never more
//! than one in flight, however fast keys come, and an answer to what's no
//! longer typed is dropped, the latest asked instead. Meanwhile what's
//! typed is lit and the cursor and the view stay where the last answer put
//! them, so they don't flicker back at each key. Nothing waits for these
//! answers. A count before `/` isn't shown till it's searched for.

use clackatoo_protocol::{Match, Request};

use super::{Document, MatchesIn, Pane, Pending, normal};
use crate::motion::{Cursor, Lines, Point};
use crate::vim::search::{self, Find, Finding, Kinds, Last, Restores, Search, Searcher};

/// The kinds a terminal's search works in: matches as the daemon finds
/// them, and what's looked for as typed, with whether it matches case.
#[derive(Debug)]
pub(crate) struct Terminal;

impl Kinds for Terminal {
    type Start = Start;
    type Found = Match;
    type Lit = (Vec<char>, bool);
    type LitKey = MatchesIn;
    type LitMatch = Match;
}

/// Where a search being typed started, to put back.
#[derive(Debug)]
pub(crate) struct Start {
    cursor: Option<Cursor>,
    /// The view's place, unless history has been numbered afresh since.
    scroll: Option<u64>,
}

/// A search asked of the daemon, a match at a time.
#[derive(Debug)]
pub(crate) struct Asked {
    query: String,
    exact: bool,
    /// Where the match last found is, which the next is asked from.
    from: Point,
    backward: bool,
    /// How many matches are still to go.
    left: usize,
    /// How many have been found, and the first.
    taken: usize,
    first: Option<Point>,
    /// Whether it went round the end on the way.
    wrapped: bool,
}

/// Where a match starts.
fn start_of(found: Match) -> Point {
    Point {
        line: found.start.line,
        col: found.start.col,
    }
}

impl Restores for Pane {
    type Kinds = Terminal;

    fn search(&mut self) -> &mut Search<Terminal> {
        &mut self.search
    }

    fn keep(&self) -> Start {
        Start {
            cursor: self.cursor,
            scroll: Some(self.scroll),
        }
    }

    /// Or if history has been numbered afresh since, the view follows the
    /// cursor.
    fn put_back(&mut self, start: &mut Start, keep_view: bool) {
        self.cursor = start.cursor;
        match start.scroll {
            _ if keep_view => {}
            Some(scroll) => {
                self.scroll = scroll.min(self.max_scroll());
                self.fetch_history();
            }
            None => self.follow_cursor(),
        }
    }
}

impl Searcher for Pane {
    const ANSWERS_LATER: bool = true;

    fn compile(&self, last: &Last) -> Result<(Vec<char>, bool), String> {
        Ok((last.query.chars().collect(), self.case.exact(&last.query)))
    }

    fn find(&mut self, find: Find<'_, Terminal>) -> Finding<Match> {
        let (query, exact) = (find.last.query.clone(), find.lit.1);
        if let Some(start) = find.preview {
            let Some(from) = start.cursor.filter(|_| find.count == 1) else {
                return Finding::Missed(String::new());
            };
            let pending = Pending::Preview(query.clone());
            self.ask_search((&query, exact), from.at, find.backward, pending);
            return Finding::Asked;
        }
        let Some(cursor) = self.cursor else {
            return Finding::Missed(format!("not found: {query}"));
        };
        self.ask_next(Asked {
            query,
            exact,
            from: cursor.at,
            backward: find.backward,
            left: find.count,
            taken: 0,
            first: None,
            wrapped: false,
        });
        Finding::Asked
    }

    fn go(&mut self, found: Match) {
        self.cursor = Some(Cursor::new(start_of(found)));
        self.follow_cursor();
    }

    fn say(&mut self, said: String) {
        self.status = Some(said);
    }

    fn fail(&mut self) {
        self.failed = true;
    }

    fn previewing(&self) -> bool {
        (self.requests.pending()).any(|pending| matches!(pending, Pending::Preview(_)))
    }
}

impl Pane {
    /// Asks the daemon where `query`, matching case if `exact`, is next
    /// from `from`, or before it if `backward`, for `pending`.
    fn ask_search(
        &mut self,
        (query, exact): (&str, bool),
        from: Point,
        backward: bool,
        pending: Pending,
    ) {
        let request = Request::Search {
            terminal: self.terminal(),
            exact,
            query: query.to_owned(),
            from: normal::position(from),
            backward,
        };
        self.request(request, pending);
    }

    /// Asks for the next match of `asked`.
    fn ask_next(&mut self, asked: Asked) {
        let (from, backward) = (asked.from, asked.backward);
        let query = (asked.query.clone(), asked.exact);
        self.ask_search((&query.0, query.1), from, backward, Pending::Search(asked));
    }

    /// Takes in where the daemon found the next match of `asked`, if
    /// anywhere: on to the next, if the count says so, or there.
    pub(super) fn searched(&mut self, found: Option<Match>, mut asked: Asked) {
        let backward = asked.backward;
        let Some(found) = found else {
            let missed = Finding::Missed(format!("not found: {}", asked.query));
            return search::answered(self, missed, backward);
        };
        let at = start_of(found);
        // A match back where it started went all the way round.
        asked.wrapped |= if backward {
            at >= asked.from
        } else {
            at <= asked.from
        };
        asked.taken += 1;
        asked.left -= 1;
        match asked.first {
            None => asked.first = Some(at),
            // Round to the first again: the rest of the count goes round
            // as many times, which ends where this many over.
            Some(first) if first == at => asked.left %= asked.taken - 1,
            Some(_) => {}
        }
        if asked.left > 0 {
            asked.from = at;
            return self.ask_next(asked);
        }
        let wrapped = asked.wrapped;
        search::answered(self, Finding::Found { at: found, wrapped }, backward);
    }

    /// Takes in where the daemon found `query`, asked of a search being
    /// typed (see [`search::previewed`]).
    pub(super) fn previewed(&mut self, query: &str, found: Option<Match>) {
        search::previewed(self, query, found);
    }

    /// Asks the daemon where the search being typed would go, as
    /// `incsearch` shows it (see [`search::preview`]). Returns whether the
    /// pane must be redrawn.
    pub(crate) fn preview_search(&mut self) -> bool {
        search::preview(self)
    }

    /// History is numbered afresh: where the search being typed started
    /// is kept, as far as the lines go now, but not the view then, nor the
    /// match found, which may be elsewhere now; and it's asked again.
    pub(super) fn search_start_renumbered(&mut self) {
        let (first, last) = Document(self).bounds();
        if let Some(start) = self.search.start_mut() {
            if let Some(cursor) = &mut start.cursor {
                let line = cursor.at.line.clamp(first, last);
                if line != cursor.at.line {
                    *cursor = Cursor::new(Point { line, col: 0 });
                }
            }
            start.scroll = None;
        }
        if let Some(found) = self.search.kept_found_mut() {
            *found = None;
        }
        self.search.show_again();
    }

    /// Keeps the view the search being typed started with on the same
    /// lines, as `lines` more have scrolled into history.
    pub(super) fn search_start_scrolled(&mut self, lines: u64) {
        if let Some(scroll) = (self.search.start_mut()).and_then(|start| start.scroll.as_mut()) {
            *scroll += lines;
        }
    }

    /// Stops lighting up the last search's matches, as `:nohlsearch` does,
    /// until the next search.
    pub(crate) fn forget_match(&mut self) {
        self.search.unlight();
    }
}
