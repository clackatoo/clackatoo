//! `/` `?` `n` `N` in a terminal's normal mode, on the search every pane
//! shares (see [`crate::vim::search`]), with `incsearch`'s preview and
//! `hlsearch`'s lit matches.
//!
//! The daemon searches, as it holds all of history, so finding is asked
//! of it and answered later, a count, as `3n`, in one answer. A daemon
//! older than protocol 108 answers the next match alone, so the rest are
//! asked of it a match at a time, each from the last, until it's back at
//! the first, when only as many as are left of a time round are asked
//! (see [`search::Steps`]). Keys typed meanwhile wait for the answer (see
//! [`Pane::waits_for_answer`]), the window's patience starting again at
//! each.
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

use super::{Document, Pane, Pending, normal};
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
}

/// Where a search being typed started, to put back.
#[derive(Debug)]
pub(crate) struct Start {
    cursor: Option<Cursor>,
    /// The view's place, unless history has been numbered afresh since.
    scroll: Option<u64>,
}

/// A search asked of the daemon.
#[derive(Debug)]
pub(crate) struct Asked {
    query: String,
    exact: bool,
    /// Where it's asked from: the cursor, or the match last found.
    from: Point,
    backward: bool,
    /// How many matches are still to go, asked of a daemon that answers
    /// a match at a time.
    steps: search::Steps<Point>,
    /// History's numbering when it was asked: numbered afresh since, its
    /// lines may be elsewhere now.
    generation: u64,
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
            self.ask_search((&query, exact), (from.at, find.backward), None, pending);
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
            steps: search::Steps::new(find.count),
            generation: self.history.generation(),
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
    /// from `from`, or before it if `backward`, or with a `count`, which
    /// match is, for `pending`.
    fn ask_search(
        &mut self,
        (query, exact): (&str, bool),
        (from, backward): (Point, bool),
        count: Option<usize>,
        pending: Pending,
    ) {
        let request = Request::Search {
            terminal: self.terminal(),
            exact,
            query: query.to_owned(),
            from: normal::position(from),
            backward,
            count: count.map(|count| u32::try_from(count).unwrap_or(u32::MAX)),
        };
        self.request(request, pending);
    }

    /// Asks for the match `asked` goes to: the count left, which a daemon
    /// older than protocol 108 answers with the next.
    fn ask_next(&mut self, asked: Asked) {
        let left = asked.steps.left();
        let (query, exact) = (asked.query.clone(), asked.exact);
        let from = (asked.from, asked.backward);
        let count = (left > 1).then_some(left);
        self.ask_search((&query, exact), from, count, Pending::Search(asked));
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
        // History numbered afresh since it was asked: where it went from
        // is numbered as it was, and it may never come back to its first
        // match, so it ends here.
        let renumbered = asked.generation != self.history.generation();
        // A match back where it started went all the way round.
        let round = !renumbered
            && if backward {
                at >= asked.from
            } else {
                at <= asked.from
            };
        if asked.steps.step(at, round) && !renumbered {
            asked.from = at;
            return self.ask_next(asked);
        }
        let wrapped = asked.steps.wrapped();
        search::answered(self, Finding::Found { at: found, wrapped }, backward);
    }

    /// Takes in where the daemon found the match `asked`'s count goes to,
    /// if anywhere, and whether it went round to it.
    pub(super) fn counted(&mut self, found: Option<Match>, wrapped: bool, asked: &Asked) {
        let finding = match found {
            Some(at) => Finding::Found {
                at,
                wrapped: wrapped || asked.steps.wrapped(),
            },
            None => Finding::Missed(format!("not found: {}", asked.query)),
        };
        search::answered(self, finding, asked.backward);
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
    /// match found, which may be elsewhere now; and it's asked again. (A
    /// count being asked a match at a time ends at its next answer: see
    /// [`searched`](Self::searched).)
    pub(super) fn search_renumbered(&mut self) {
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

    /// Puts `text` into the search being typed, if one is, as the command
    /// line takes a paste. Returns whether one was.
    pub(crate) fn paste_into_search(&mut self, text: &str) -> bool {
        self.search.paste(text)
    }

    /// Stops lighting up the last search's matches, as `:nohlsearch` does,
    /// until the next search.
    pub(crate) fn forget_match(&mut self) {
        self.search.unlight();
    }
}
