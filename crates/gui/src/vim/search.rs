//! `/` and `?`, `n` and `N`, done once for every pane: editors, terminals'
//! normal mode, chats, and lists (explorers, boards, reviews).
//!
//! What's shared is here, in [`Search`] and the functions that drive it:
//! - the search typed in the bar, edited as the command line is (see
//!   [`crate::cmdline`]), with the searches typed before for Up and Down
//!   and Ctrl+R for a register (the desk has those, through
//!   [`Search::line`]);
//! - which way it goes, `/` or `?`, and a count typed before it, as `3/x`
//!   goes to the third;
//! - Esc, which gives it up, and Enter, which makes it the last search, or
//!   with nothing typed, searches for the last again, this way;
//! - the last search, for `n` and `N`, with a count;
//! - `incsearch`: where it started, kept to put back, and the preview of
//!   where it goes, worked out as the window settles before it draws, not
//!   at each key, so a macro's `/foo<CR>`, typed within one, shows none. A
//!   preview is always where Enter would go, or nothing; Esc puts the
//!   cursor, the view and what was lit back, and Enter puts the cursor
//!   back and then searches from there, as without `incsearch`;
//! - `hlsearch`: the search whose matches are lit, the match it went to,
//!   and what was found lit in view, put out by Esc and `:nohlsearch` (see
//!   [`crate::hlsearch`]);
//! - what the bar says, and a search failing, as Vim beeps, stopping a
//!   macro (see [`crate::motion::Motion::failed`]).
//!
//! What differs, each pane supplies, as a [`Searcher`]: how it finds the
//! `count`th match from a place, which may be answered later, as the
//! daemon answers a terminal's; how it goes there; what it keeps of its
//! cursor and view to put back; and how a search is matched and lit in
//! what it shows.

use std::fmt::Debug;

use super::{Stroke, WRAPPED_TO_BOTTOM, WRAPPED_TO_TOP};
use crate::cmdline::controls::Control;
use crate::cmdline::{Keyed, Line};
use crate::hlsearch::Memo;

/// The types a pane's search works in.
pub(crate) trait Kinds: Debug {
    /// What the pane keeps as a search starts being typed, to put back:
    /// its cursor and its view.
    type Start: Debug;
    /// Where a match is, as the pane goes to it and lights it brighter.
    type Found: Copy + Debug;
    /// A search as the pane matches it, compiled once.
    type Lit: Clone + Debug;
    /// What the matches lit in view were last found in: which search, and
    /// what's drawn where.
    type LitKey: PartialEq + Debug;
    /// A match lit in view.
    type LitMatch: Clone + Debug;
}

/// A search done: what it looks for, and which way it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Last {
    /// What was typed, as Vim writes a pattern.
    pub(crate) query: String,
    pub(crate) backward: bool,
    /// Whether it's a word's, as `*` and `#` search: case is matched as
    /// `ignorecase` says, never as `smartcase`, as in Vim.
    pub(crate) word: bool,
}

impl Last {
    /// A search for `query`, typed, going backward if `backward`.
    pub(crate) fn typed(query: impl Into<String>, backward: bool) -> Self {
        Self {
            query: query.into(),
            backward,
            word: false,
        }
    }
}

/// A pane's search: the one being typed, the last one, and what's lit.
#[derive(Debug)]
pub(crate) struct Search<K: Kinds> {
    typing: Option<Typing<K>>,
    last: Option<Last>,
    /// The search whose matches are lit: from a search, `n` or `N` until
    /// Esc or `:nohlsearch`.
    lit: Option<K::Lit>,
    /// The match the last search, `n` or `N` went to, while it's lit.
    found: Option<K::Found>,
    /// What was last found lit in view, and in what.
    lit_kept: Memo<K::LitKey, K::LitMatch>,
    /// Every match in view is lit, not only the one found (`hlsearch`).
    pub(crate) every: bool,
    /// A search being typed shows where it goes (`incsearch`).
    pub(crate) incsearch: bool,
}

impl<K: Kinds> Default for Search<K> {
    fn default() -> Self {
        let options = crate::options::Options::default();
        Self {
            typing: None,
            last: None,
            lit: None,
            found: None,
            lit_kept: Memo::default(),
            every: options.hlsearch,
            incsearch: options.incsearch,
        }
    }
}

/// A search being typed in the bar.
#[derive(Debug)]
struct Typing<K: Kinds> {
    line: Line,
    backward: bool,
    /// The count typed before `/` or `?`.
    count: Option<usize>,
    /// Where it started, with `incsearch`.
    start: Option<Started<K>>,
    /// What was typed when it was last shown, or asked where it goes.
    previewed: Option<String>,
}

/// What a search being typed keeps to put back.
#[derive(Debug)]
struct Started<K: Kinds> {
    start: K::Start,
    lit: Option<K::Lit>,
    found: Option<K::Found>,
}

/// What a pane is asked to find.
#[derive(Debug)]
pub(crate) struct Find<'a, K: Kinds> {
    pub(crate) last: &'a Last,
    /// It compiled, as [`Searcher::compile`] made it.
    pub(crate) lit: &'a K::Lit,
    /// Which way it goes, which isn't the last's after `N`.
    pub(crate) backward: bool,
    /// Which match, after the place it's found from: 1 for the next.
    pub(crate) count: usize,
    /// `None` for a search, from the cursor. For a preview, where the
    /// search being typed started, to find from: a preview never fails or
    /// says anything, and finds what the search Enter makes would find, or
    /// nothing.
    pub(crate) preview: Option<&'a mut K::Start>,
}

/// What finding a match found.
#[derive(Debug)]
pub(crate) enum Finding<F> {
    /// It's here, and the search went round the pane's end to it.
    Found { at: F, wrapped: bool },
    /// There's none, for the reason the bar gives: for a preview, none
    /// that it can be sure of.
    Missed(String),
    /// The pane will answer later, through [`answered`] or [`previewed`].
    Asked,
}

/// A pane a search is typed in: what it keeps of its cursor and view as
/// one starts, and puts back, which is all that starting one and giving it
/// up need.
pub(crate) trait Restores {
    type Kinds: Kinds;

    /// Its search.
    fn search(&mut self) -> &mut Search<Self::Kinds>;

    /// Its cursor and view, to put back.
    fn keep(&self) -> <Self::Kinds as Kinds>::Start;

    /// Puts the cursor back as `start` keeps it, and the view, unless
    /// `keep_view`, as Enter keeps it.
    fn put_back(&mut self, start: &mut <Self::Kinds as Kinds>::Start, keep_view: bool);
}

/// A pane that's searched.
pub(crate) trait Searcher: Restores {
    /// Whether the pane's finds are answered later, as the daemon answers
    /// a terminal's: then the last preview stays shown, the cursor and the
    /// view where it put them, till the next is answered, so they don't
    /// flicker back at each key.
    const ANSWERS_LATER: bool = false;

    /// `last` compiled, or why it can't be.
    fn compile(&self, last: &Last) -> Result<<Self::Kinds as Kinds>::Lit, String>;

    /// Finds what `find` says.
    fn find(&mut self, find: Find<'_, Self::Kinds>) -> Finding<<Self::Kinds as Kinds>::Found>;

    /// Puts the cursor on the match `found`, the view following.
    fn go(&mut self, found: <Self::Kinds as Kinds>::Found);

    /// Says `said` in the bar till the next key.
    fn say(&mut self, said: String);

    /// Notes that a command failed, as Vim beeps.
    fn fail(&mut self);

    /// Whether the pane is still answering where a search being typed goes,
    /// as no more is asked meanwhile.
    fn previewing(&self) -> bool {
        false
    }
}

impl<K: Kinds> Search<K> {
    /// A pane's search, as `options` say it's lit and shown.
    pub(crate) fn of(options: &crate::options::Options) -> Self {
        let mut search = Self::default();
        search.set_options(options);
        search
    }

    /// Takes in how searches are lit and shown, as the options say.
    pub(crate) fn set_options(&mut self, options: &crate::options::Options) {
        self.every = options.hlsearch;
        self.incsearch = options.incsearch;
    }

    /// Whether a search is being typed, which takes every key.
    pub(crate) fn typing(&self) -> bool {
        self.typing.is_some()
    }

    /// The search being typed, for the desk to recall a search typed
    /// before into, or put a register in.
    pub(crate) fn line(&mut self) -> Option<&mut Line> {
        self.typing.as_mut().map(|typing| &mut typing.line)
    }

    /// What the bar says while a search is typed: `/` or `?`, and what's
    /// typed.
    pub(crate) fn label(&self) -> Option<String> {
        let typing = self.typing.as_ref()?;
        let slash = if typing.backward { '?' } else { '/' };
        Some(format!("{slash}{}", typing.line.shown()))
    }

    /// The keys Ctrl+? shows while a search is typed, under `title`.
    pub(crate) fn shortcuts(&self, title: &str) -> Option<crate::shortcuts::Context> {
        let typing = self.typing.as_ref()?;
        Some(crate::shortcuts::prompt(title, &typing.line, true))
    }

    /// Pastes `text` into the search being typed, if one is, on one line.
    /// Returns whether one was.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        let Some(line) = self.line() else {
            return false;
        };
        line.put(text);
        true
    }

    /// What the pane kept as the search being typed started, if it's kept
    /// one, to follow what's changed since.
    pub(crate) fn start_mut(&mut self) -> Option<&mut K::Start> {
        let started = self.typing.as_mut()?.start.as_mut()?;
        Some(&mut started.start)
    }

    /// The match lit as the search being typed started, to be put back,
    /// if one's kept.
    pub(crate) fn kept_found_mut(&mut self) -> Option<&mut Option<K::Found>> {
        let started = self.typing.as_mut()?.start.as_mut()?;
        Some(&mut started.found)
    }

    /// Has the search being typed shown afresh, as what it was shown in
    /// has changed.
    pub(crate) fn show_again(&mut self) {
        if let Some(typing) = &mut self.typing {
            typing.previewed = None;
        }
    }

    /// The last search, for `n` and `N`.
    pub(crate) fn last(&self) -> Option<&Last> {
        self.last.as_ref()
    }

    /// Makes `last` the last search, as `:s` and `:g` make theirs.
    pub(crate) fn set_last(&mut self, last: Option<Last>) {
        self.last = last;
    }

    /// The search whose matches are lit, if they are.
    pub(crate) fn lit(&self) -> Option<&K::Lit> {
        self.lit.as_ref()
    }

    /// Lights the matches of `lit`, or none.
    pub(crate) fn set_lit(&mut self, lit: Option<K::Lit>) {
        self.lit = lit;
    }

    /// The match the last search went to, while it's lit.
    pub(crate) fn found(&self) -> Option<K::Found> {
        self.found
    }

    /// The match the last search went to, to move with what's moved.
    pub(crate) fn found_mut(&mut self) -> &mut Option<K::Found> {
        &mut self.found
    }

    /// Stops lighting the last search's matches, as Esc and `:nohlsearch`
    /// do, until the next search.
    pub(crate) fn unlight(&mut self) {
        self.lit = None;
        self.found = None;
        self.lit_kept.forget();
    }

    /// The matches lit in view, with `hlsearch` and while they're lit, as
    /// `find` finds them for the search lit: kept, while `key` says they're
    /// in what they were found in last. The one found isn't among them
    /// unless `find` puts it there.
    pub(crate) fn lit_in_view(
        &self,
        key: impl FnOnce(&K::Lit) -> K::LitKey,
        find: impl FnOnce(&K::Lit) -> Vec<K::LitMatch>,
    ) -> Vec<K::LitMatch> {
        match self.lit.as_ref().filter(|_| self.every) {
            Some(lit) => self.lit_kept.get(key(lit), || find(lit)),
            None => Vec::new(),
        }
    }
}

/// `/`, or `?` if `backward`, `count` typed before it: starts typing a
/// search in the bar. With `incsearch`, where it starts is kept, to show
/// it from and put back.
pub(crate) fn start<P: Restores>(pane: &mut P, backward: bool, count: Option<usize>) {
    give_up(pane);
    let start = pane.search().incsearch.then(|| pane.keep());
    let search = pane.search();
    let start = start.map(|start| Started {
        start,
        lit: search.lit.clone(),
        found: search.found,
    });
    search.typing = Some(Typing {
        line: Line::default(),
        backward,
        count,
        start,
        previewed: None,
    });
}

/// What a key typed into a search did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Typed {
    /// None is being typed: the key isn't the search's.
    Not,
    /// It was typed into it.
    Edited,
    /// It gave the search up.
    GaveUp,
    /// Enter: it searched.
    Searched,
}

/// Takes `stroke` for the search being typed, if one is: Enter searches,
/// and Esc, or Backspace with nothing typed, gives it up.
pub(crate) fn key<P: Searcher>(pane: &mut P, stroke: Stroke) -> Typed {
    let Some(typing) = &mut pane.search().typing else {
        return Typed::Not;
    };
    match Control::read(stroke).0 {
        Control::Accept => {
            accept(pane);
            Typed::Searched
        }
        Control::Cancel => {
            give_up(pane);
            Typed::GaveUp
        }
        Control::Other if typing.line.stroke(stroke) == Keyed::Emptied => {
            give_up(pane);
            Typed::GaveUp
        }
        Control::Other => Typed::Edited,
    }
}

/// Enter: the search typed is the last now, or with nothing typed, the
/// last again, the way this one goes, as in Vim; it's searched for from
/// where it started, the view kept.
fn accept<P: Searcher>(pane: &mut P) {
    let Some(mut typing) = pane.search().typing.take() else {
        return;
    };
    put_back(pane, typing.start.take(), true);
    let query = typing.line.take();
    let search = pane.search();
    match &mut search.last {
        _ if !query.is_empty() => search.last = Some(Last::typed(query, typing.backward)),
        Some(last) => last.backward = typing.backward,
        None => {}
    }
    again(pane, false, typing.count.unwrap_or(1));
}

/// Gives up the search being typed, if one is, as Esc and leaving the pane
/// do: the cursor, the view and what was lit are put back.
pub(crate) fn give_up<P: Restores>(pane: &mut P) {
    if let Some(typing) = pane.search().typing.take() {
        put_back(pane, typing.start, false);
    }
}

/// Puts back what `started` kept: the cursor, what was lit, and the view
/// unless `keep_view`.
fn put_back<P: Restores>(pane: &mut P, started: Option<Started<P::Kinds>>, keep_view: bool) {
    let Some(mut started) = started else {
        return;
    };
    pane.put_back(&mut started.start, keep_view);
    let search = pane.search();
    search.lit = started.lit;
    search.found = started.found;
}

/// Puts back what the search being typed kept, keeping it to show from.
fn put_back_kept<P: Restores>(pane: &mut P) {
    let Some(typing) = &mut pane.search().typing else {
        return;
    };
    let Some(mut started) = typing.start.take() else {
        return;
    };
    pane.put_back(&mut started.start, false);
    let search = pane.search();
    (search.lit, search.found) = (started.lit.clone(), started.found);
    if let Some(typing) = &mut search.typing {
        typing.start = Some(started);
    }
}

/// `n`, or `N` if `reverse`: the `count`th match of the last search after
/// the cursor, the way it went, or back. Its matches are lit from now,
/// found or not.
pub(crate) fn again<P: Searcher>(pane: &mut P, reverse: bool, count: usize) {
    let Some(last) = pane.search().last.clone() else {
        // As Vim's E35.
        pane.say("no search yet".to_owned());
        pane.fail();
        return;
    };
    let lit = match pane.compile(&last) {
        Ok(lit) => lit,
        Err(why) => {
            pane.say(why);
            pane.fail();
            return;
        }
    };
    pane.search().lit = Some(lit.clone());
    let backward = last.backward != reverse;
    let finding = pane.find(Find {
        last: &last,
        lit: &lit,
        backward,
        count: count.max(1),
        preview: None,
    });
    answered(pane, finding, backward);
}

/// Searches for `last`, the `count`th match from the cursor, making it the
/// last search, as `*` does.
pub(crate) fn search_for<P: Searcher>(pane: &mut P, last: Last, count: usize) {
    pane.search().last = Some(last);
    again(pane, false, count);
}

/// Takes in what a search found, going backward if `backward`: the cursor
/// goes to the match, saying so if the search went round the pane's end;
/// or with none, the bar says why, and the search fails.
pub(crate) fn answered<P: Searcher>(
    pane: &mut P,
    finding: Finding<<P::Kinds as Kinds>::Found>,
    backward: bool,
) {
    match finding {
        Finding::Found { at, wrapped } => {
            pane.search().found = Some(at);
            pane.go(at);
            if wrapped {
                let said = if backward {
                    WRAPPED_TO_BOTTOM
                } else {
                    WRAPPED_TO_TOP
                };
                pane.say(said.to_owned());
            }
        }
        Finding::Missed(why) => {
            pane.search().found = None;
            pane.say(why);
            pane.fail();
        }
        Finding::Asked => {}
    }
}

/// Shows where the search being typed would go, with `incsearch`, if
/// what's typed has changed since it was last shown: from where it
/// started, the cursor going to the match, the view following, and its
/// matches lit. Nothing typed puts it all back; a pattern that isn't one
/// yet lights nothing; and none found that's sure to be what Enter finds,
/// as where the search started. Returns whether the pane must be drawn
/// again.
pub(crate) fn preview<P: Searcher>(pane: &mut P) -> bool {
    let previewing = pane.previewing();
    let Some(typing) = &mut pane.search().typing else {
        return false;
    };
    if typing.start.is_none() || typing.previewed.as_deref() == Some(typing.line.text()) {
        return false;
    }
    // One at a time, the next once this is answered.
    if previewing {
        return false;
    }
    let typed = typing.line.text().to_owned();
    typing.previewed = Some(typed.clone());
    let (backward, count) = (typing.backward, typing.count.unwrap_or(1));
    if !P::ANSWERS_LATER || typed.is_empty() {
        put_back_kept(pane);
    }
    if typed.is_empty() {
        return true;
    }
    let last = Last::typed(typed, backward);
    pane.search().found = None;
    let Ok(lit) = pane.compile(&last) else {
        pane.search().lit = None;
        return true;
    };
    let search = pane.search();
    search.lit = Some(lit.clone());
    let Some(mut started) = search
        .typing
        .as_mut()
        .and_then(|typing| typing.start.take())
    else {
        return true;
    };
    let finding = pane.find(Find {
        last: &last,
        lit: &lit,
        backward,
        count,
        preview: Some(&mut started.start),
    });
    if let Some(typing) = &mut pane.search().typing {
        typing.start = Some(started);
    }
    match finding {
        Finding::Found { at, .. } => shown(pane, Some(at)),
        Finding::Missed(_) => shown(pane, None),
        Finding::Asked => {}
    }
    true
}

/// Takes in where the search being typed, `query`, would go, asked of a
/// pane that answers later, if it's still what's typed, and else drops it.
pub(crate) fn previewed<P: Searcher>(
    pane: &mut P,
    query: &str,
    found: Option<<P::Kinds as Kinds>::Found>,
) {
    let Some(typing) = &pane.search().typing else {
        return;
    };
    if typing.line.text() != query || typing.previewed.as_deref() != Some(query) {
        return;
    }
    shown(pane, found);
}

/// Shows what a preview found: the cursor goes to the match, or with none,
/// back where the search started, its matches still lit.
fn shown<P: Searcher>(pane: &mut P, found: Option<<P::Kinds as Kinds>::Found>) {
    if let Some(at) = found {
        pane.search().found = Some(at);
        pane.go(at);
        return;
    }
    let lit = pane.search().lit.take();
    put_back_kept(pane);
    let search = pane.search();
    (search.lit, search.found) = (lit, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane of numbered places, the matches at `matches`, each place a
    /// line, the view `top` and as tall as it likes.
    #[derive(Debug, Default)]
    struct Places {
        search: Search<Places>,
        matches: Vec<usize>,
        cursor: usize,
        top: usize,
        said: Option<String>,
        failed: bool,
        /// A preview or a search asked, answered later.
        later: bool,
        asked: Vec<(usize, bool, usize, bool)>,
    }

    impl Kinds for Places {
        type Start = (usize, usize);
        type Found = usize;
        type Lit = String;
        type LitKey = String;
        type LitMatch = usize;
    }

    impl Restores for Places {
        type Kinds = Self;

        fn search(&mut self) -> &mut Search<Self> {
            &mut self.search
        }
        fn keep(&self) -> (usize, usize) {
            (self.cursor, self.top)
        }
        fn put_back(&mut self, start: &mut (usize, usize), keep_view: bool) {
            self.cursor = start.0;
            if !keep_view {
                self.top = start.1;
            }
        }
    }

    impl Searcher for Places {
        fn compile(&self, last: &Last) -> Result<String, String> {
            if last.query.contains('\\') {
                return Err("not a pattern".into());
            }
            Ok(last.query.clone())
        }
        fn find(&mut self, find: Find<'_, Self>) -> Finding<usize> {
            let real = find.preview.is_none();
            let from = find.preview.map_or(self.cursor, |start| start.0);
            if self.later {
                self.asked.push((from, find.backward, find.count, real));
                return Finding::Asked;
            }
            match super::super::list::round(1000, from, find.count, find.backward, |at| {
                find.lit != "none" && self.matches.contains(&at)
            }) {
                Some(at) => Finding::Found {
                    at,
                    wrapped: if find.backward { at > from } else { at < from },
                },
                None => Finding::Missed(format!("not found: {}", find.last.query)),
            }
        }
        fn go(&mut self, found: usize) {
            self.cursor = found;
            self.top = found;
        }
        fn say(&mut self, said: String) {
            self.said = Some(said);
        }
        fn fail(&mut self) {
            self.failed = true;
        }
    }

    fn places(matches: &[usize], cursor: usize) -> Places {
        Places {
            matches: matches.to_vec(),
            cursor,
            top: cursor,
            ..Places::default()
        }
    }

    fn typed(pane: &mut Places, text: &str) {
        for ch in text.chars() {
            assert_eq!(key(pane, Stroke::char(ch)), Typed::Edited);
        }
    }

    #[test]
    fn enter_searches_from_where_it_started_and_esc_puts_it_all_back() {
        let mut pane = places(&[3, 7, 12], 5);
        start(&mut pane, false, None);
        typed(&mut pane, "x");
        assert_eq!(pane.search.label().as_deref(), Some("/x▏"));
        assert!(preview(&mut pane));
        assert_eq!(
            (pane.cursor, pane.top, pane.search.found()),
            (7, 7, Some(7))
        );
        // Nothing new typed, nothing done.
        assert!(!preview(&mut pane));
        assert_eq!(key(&mut pane, Stroke::Escape), Typed::GaveUp);
        assert_eq!((pane.cursor, pane.top, pane.search.found()), (5, 5, None));
        assert!(pane.search.lit().is_none());
        // Enter finds what was shown, the view kept.
        start(&mut pane, false, None);
        typed(&mut pane, "x");
        preview(&mut pane);
        assert_eq!(key(&mut pane, Stroke::Enter), Typed::Searched);
        assert_eq!((pane.cursor, pane.search.found()), (7, Some(7)));
        assert_eq!(pane.search.last(), Some(&Last::typed("x", false)));
        assert_eq!(pane.search.lit().map(String::as_str), Some("x"));
    }

    #[test]
    fn a_count_before_the_slash_goes_as_far_in_the_preview_and_the_search() {
        let mut pane = places(&[3, 7, 12], 0);
        start(&mut pane, false, Some(2));
        typed(&mut pane, "x");
        preview(&mut pane);
        assert_eq!(pane.cursor, 7);
        key(&mut pane, Stroke::Enter);
        assert_eq!(pane.cursor, 7);
        again(&mut pane, false, 2);
        assert_eq!(
            (pane.cursor, pane.said.as_deref()),
            (3, Some(WRAPPED_TO_TOP))
        );
        again(&mut pane, true, 1);
        assert_eq!(pane.cursor, 12);
    }

    #[test]
    fn enter_with_nothing_typed_is_the_last_again_this_way() {
        let mut pane = places(&[3, 7], 5);
        start(&mut pane, false, None);
        key(&mut pane, Stroke::Enter);
        assert_eq!(
            (pane.said.as_deref(), pane.failed),
            (Some("no search yet"), true)
        );
        start(&mut pane, false, None);
        typed(&mut pane, "x");
        key(&mut pane, Stroke::Enter);
        assert_eq!(pane.cursor, 7);
        start(&mut pane, true, None);
        key(&mut pane, Stroke::Enter);
        assert_eq!(pane.cursor, 3);
        assert_eq!(pane.search.last(), Some(&Last::typed("x", true)));
    }

    #[test]
    fn a_miss_fails_and_a_preview_of_none_shows_nothing() {
        let mut pane = places(&[3], 5);
        start(&mut pane, false, None);
        typed(&mut pane, "none");
        preview(&mut pane);
        assert_eq!(
            (pane.cursor, pane.failed, pane.said.as_deref()),
            (5, false, None)
        );
        // Lit, though nothing's found.
        assert_eq!(pane.search.lit().map(String::as_str), Some("none"));
        key(&mut pane, Stroke::Enter);
        assert_eq!(
            (pane.failed, pane.said.as_deref()),
            (true, Some("not found: none"))
        );
        // A pattern that isn't one lights nothing.
        start(&mut pane, false, None);
        typed(&mut pane, "\\");
        preview(&mut pane);
        assert!(pane.search.lit().is_none());
        // Backspace with nothing typed gives up.
        key(&mut pane, Stroke::Backspace);
        assert_eq!(key(&mut pane, Stroke::Backspace), Typed::GaveUp);
        assert!(!pane.search.typing());
        assert_eq!(pane.search.lit().map(String::as_str), Some("none"));
    }

    #[test]
    fn without_incsearch_nothing_is_shown_or_kept() {
        let mut pane = places(&[7], 5);
        pane.search.incsearch = false;
        start(&mut pane, false, None);
        typed(&mut pane, "x");
        assert!(!preview(&mut pane));
        assert_eq!(pane.cursor, 5);
        key(&mut pane, Stroke::Enter);
        assert_eq!(pane.cursor, 7);
    }

    /// [`Places`] answering later, as a terminal's daemon does.
    #[derive(Debug, Default)]
    struct Later(Places);

    impl Restores for Later {
        type Kinds = Places;

        fn search(&mut self) -> &mut Search<Places> {
            &mut self.0.search
        }
        fn keep(&self) -> (usize, usize) {
            self.0.keep()
        }
        fn put_back(&mut self, start: &mut (usize, usize), keep_view: bool) {
            self.0.put_back(start, keep_view);
        }
    }

    impl Searcher for Later {
        const ANSWERS_LATER: bool = true;

        fn compile(&self, last: &Last) -> Result<String, String> {
            self.0.compile(last)
        }
        fn find(&mut self, find: Find<'_, Places>) -> Finding<usize> {
            self.0.find(find)
        }
        fn go(&mut self, found: usize) {
            self.0.go(found);
        }
        fn say(&mut self, said: String) {
            self.0.say(said);
        }
        fn fail(&mut self) {
            self.0.fail();
        }
        fn previewing(&self) -> bool {
            self.0.asked.iter().any(|asked| !asked.3)
        }
    }

    #[test]
    fn answers_that_come_later_keep_the_last_shown_and_drop_the_stale() {
        let mut pane = Later(places(&[], 5));
        pane.0.later = true;
        start(&mut pane, false, None);
        for ch in "ab".chars() {
            key(&mut pane, Stroke::char(ch));
        }
        assert!(preview(&mut pane));
        assert_eq!(pane.0.asked, [(5, false, 1, false)]);
        // One in flight: no more asked till it's answered.
        key(&mut pane, Stroke::char('c'));
        assert!(!preview(&mut pane));
        pane.0.asked.clear();
        // The answer to what's no longer typed is dropped.
        previewed(&mut pane, "ab", Some(9));
        assert_eq!(pane.0.cursor, 5);
        assert!(preview(&mut pane));
        previewed(&mut pane, "abc", Some(9));
        assert_eq!(pane.0.cursor, 9);
        // Meanwhile the next stays where the last put it.
        pane.0.asked.clear();
        key(&mut pane, Stroke::char('d'));
        preview(&mut pane);
        assert_eq!((pane.0.cursor, pane.0.top), (9, 9));
        previewed(&mut pane, "abcd", None);
        assert_eq!((pane.0.cursor, pane.0.top), (5, 5));
        assert_eq!(pane.0.search.lit().map(String::as_str), Some("abcd"));
        // Enter asks for real, from where it started, and its answer lands.
        pane.0.asked.clear();
        key(&mut pane, Stroke::Enter);
        assert_eq!(pane.0.asked, [(5, false, 1, true)]);
        answered(
            &mut pane,
            Finding::Found {
                at: 2,
                wrapped: true,
            },
            false,
        );
        assert_eq!(
            (pane.0.cursor, pane.0.said.as_deref()),
            (2, Some(WRAPPED_TO_TOP))
        );
    }

    #[test]
    fn what_is_lit_is_kept_till_the_search_or_what_is_drawn_changes() {
        let mut pane = places(&[7], 5);
        let lit = |pane: &Places, view: &str| {
            pane.search
                .lit_in_view(|lit| format!("{lit} {view}"), |_| vec![1, 2])
        };
        assert!(lit(&pane, "a").is_empty());
        search_for(&mut pane, Last::typed("x", false), 1);
        let scans = crate::hlsearch::SCANS.with(std::cell::Cell::get);
        assert_eq!(lit(&pane, "a"), [1, 2]);
        assert_eq!(lit(&pane, "a"), [1, 2]);
        assert_eq!(crate::hlsearch::SCANS.with(std::cell::Cell::get), scans + 1);
        pane.search.every = false;
        assert!(lit(&pane, "a").is_empty());
        pane.search.every = true;
        pane.search.unlight();
        assert!(lit(&pane, "a").is_empty());
        assert_eq!(pane.search.found(), None);
    }
}
