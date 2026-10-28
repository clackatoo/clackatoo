//! Panes of lists, on the grammar every pane reads keys with: an
//! explorer's files, a board's entries, a review's lines. The cursor is on
//! an item, not a character, so of Vim's motions they take those that go
//! between lines, `j` `k` `gg` `G` `H` `M` `L`, with counts, and the
//! scrolling keys, Ctrl+E Ctrl+Y Ctrl+D Ctrl+U Ctrl+F and the pages, and
//! `zt` `zz` `zb`, all done here once for every list ([`go`]); and `/` `?`
//! `n` `N`, searching the items shown ([`Searching`]). The rest of their
//! keys are their own, through their [`Dialect`](super::Dialect)'s hooks.

use std::ops::Range;

use super::search::{Find, Finding, Kinds, Last, Restores, Search, Searcher};
use super::{Action, Command, Keys, Paging, Scroll, Stroke, ViewAt};
use crate::editor::{Matching, Pattern};
use crate::motion::{Motion, Words};
use crate::options::Case;

/// A list the cursor moves over, item by item, and its view.
pub(crate) trait ListRows {
    /// How many items it has.
    fn len(&self) -> usize;

    /// The item the cursor's on.
    fn cursor(&self) -> usize;

    /// Puts the cursor on item `item`, which is one it has.
    fn set_cursor(&mut self, item: usize);

    /// The first item in view.
    fn top(&self) -> usize;

    /// Puts item `item`, one it has, at the top of the view.
    fn set_top(&mut self, item: usize);

    /// The items shown whole in the view, the top's first: as many as fit
    /// from [`top`](Self::top), and at least that one.
    fn shown(&self) -> Range<usize>;
}

/// Whether `command` is one [`go`] does: a list takes these of the
/// grammar's, and no other but its own.
pub(crate) fn moves<O>(command: &Command<O>) -> bool {
    match command.action {
        Action::Move(motion) => matches!(
            motion,
            Motion::Up(_)
                | Motion::Down(_)
                | Motion::RowUp(_)
                | Motion::RowDown(_)
                | Motion::First
                | Motion::Last
                | Motion::ViewTop
                | Motion::ViewMiddle
                | Motion::ViewBottom
        ),
        Action::Scroll(_) | Action::View(_) => true,
        _ => false,
    }
}

/// Does `command` in `list`, if it's one of those [`moves`] says a list
/// takes, scrolling as far as `paging` says: returns whether it was.
pub(crate) fn go<O>(list: &mut impl ListRows, paging: &mut Paging, command: &Command<O>) -> bool {
    if !moves(command) {
        return false;
    }
    // The view as it's drawn, with the cursor in it, as a pane settles it
    // only when it's drawn: keys typed in one go come before that.
    follow(list);
    let final_item = list.len().saturating_sub(1);
    let count = command.count;
    let times = command.times();
    let cursor = list.cursor().min(final_item);
    match command.action {
        Action::Move(Motion::Down(_) | Motion::RowDown(_)) => {
            list.set_cursor(cursor.saturating_add(times).min(final_item));
        }
        Action::Move(Motion::Up(_) | Motion::RowUp(_)) => {
            list.set_cursor(cursor.saturating_sub(times));
        }
        // With a count, the item numbered so, counting from 1, as `5G`
        // goes to line 5.
        Action::Move(Motion::First) => list.set_cursor(count.map_or(0, |n| n - 1).min(final_item)),
        Action::Move(Motion::Last) => {
            list.set_cursor(count.map_or(final_item, |n| n - 1).min(final_item));
        }
        Action::Move(motion @ (Motion::ViewTop | Motion::ViewMiddle | Motion::ViewBottom)) => {
            let shown = list.shown();
            let (first, bottom) = (shown.start, shown.end.saturating_sub(1).max(shown.start));
            let from_edge = times - 1;
            let item = match motion {
                Motion::ViewTop => first.saturating_add(from_edge).min(bottom),
                Motion::ViewBottom => bottom.saturating_sub(from_edge).max(first),
                _ => first + (bottom - first) / 2,
            };
            list.set_cursor(item.min(final_item));
        }
        Action::Scroll(scroll) => scrolled(list, paging, scroll, count),
        Action::View(at) => {
            // `5zt`: item 5 first, as Vim's.
            let cursor = count.map_or(cursor, |n| (n - 1).min(final_item));
            list.set_cursor(cursor);
            let shown = list.shown().len().max(1);
            let top = match at {
                ViewAt::Top => cursor,
                ViewAt::Middle => cursor.saturating_sub((shown - 1) / 2),
                ViewAt::Bottom => (cursor + 1).saturating_sub(shown),
            };
            list.set_top(top);
        }
        _ => return false,
    }
    true
}

/// Scrolls `list` `lines` items down, or up if fewer than none, as the
/// wheel does, the cursor kept in view. Returns whether the view moved.
pub(crate) fn wheel(list: &mut impl ListRows, lines: i64) -> bool {
    let top = list.top();
    let by = usize::try_from(lines.unsigned_abs()).unwrap_or(usize::MAX);
    let final_item = list.len().saturating_sub(1);
    list.set_top(if lines < 0 {
        top.saturating_sub(by)
    } else {
        top.saturating_add(by).min(final_item)
    });
    if list.top() == top {
        return false;
    }
    let shown = list.shown();
    let bottom = shown.end.saturating_sub(1).max(shown.start);
    // In order, not clamped: a view not yet laid out again may start past
    // the list's end.
    list.set_cursor(list.cursor().max(shown.start).min(bottom).min(final_item));
    true
}

/// Scrolls `list` as little as puts the cursor's item in view.
fn follow<L: ListRows + ?Sized>(list: &mut L) {
    let shown = list.shown();
    let cursor = list.cursor();
    if cursor < shown.start {
        list.set_top(cursor);
    } else if cursor >= shown.end {
        list.set_top(shown.start + (cursor + 1 - shown.end));
    }
}

/// Scrolls `list` as `scroll` does, `count` typed: the view a line, the
/// cursor kept in it, or the view and the cursor half a view, or the view
/// a page, the cursor to its top going down or its bottom going up; at
/// the list's end, where the view goes no further, the cursor goes as far
/// as it would have, as Vim's does.
fn scrolled(list: &mut impl ListRows, paging: &mut Paging, scroll: Scroll, count: Option<usize>) {
    let (Scroll::Line { down } | Scroll::HalfPage { down } | Scroll::Page { down }) = scroll;
    let final_item = list.len().saturating_sub(1);
    let view = list.shown().len().max(1) as u64;
    let by = usize::try_from(paging.rows(scroll, count, view)).unwrap_or(usize::MAX);
    let top = list.top();
    let moved = if down {
        top.saturating_add(by).min(final_item)
    } else {
        top.saturating_sub(by)
    };
    list.set_top(moved);
    let cursor = list.cursor().min(final_item);
    let shown = list.shown();
    let (first, bottom) = (shown.start, shown.end.saturating_sub(1).max(shown.start));
    let cursor = match scroll {
        Scroll::Line { .. } => cursor,
        Scroll::HalfPage { .. } if down => cursor.saturating_add(by).min(final_item),
        Scroll::HalfPage { .. } => cursor.saturating_sub(by),
        // Nowhere further to go: to the end the page was going to.
        Scroll::Page { .. } if list.top() == top => {
            if down {
                final_item
            } else {
                0
            }
        }
        Scroll::Page { .. } if down => first,
        Scroll::Page { .. } => bottom,
    };
    // In order, not clamped: a view not yet laid out again, as after the
    // list shrank, may start past its end.
    list.set_cursor(cursor.max(first).min(bottom).min(final_item));
}

/// The kinds a list's search works in: the cursor and the top as items,
/// the item a match is in, and Vim's patterns.
#[derive(Debug)]
pub(crate) struct Items;

impl Kinds for Items {
    type Start = (usize, usize);
    type Found = usize;
    type Lit = Pattern;
    // A list lights what it draws as it draws it, a view of short rows.
    type LitKey = ();
    type LitMatch = ();
}

/// A list that's searched, item by item (see [`Searching`]).
pub(crate) trait Searched: ListRows + super::ListKeys {
    /// Its search.
    fn search(&self) -> &Search<Items>;

    /// Its search, to change.
    fn search_mut(&mut self) -> &mut Search<Items>;

    /// Whether item `item` holds a match of `pattern`.
    fn matches(&self, item: usize, pattern: &Pattern) -> bool;

    /// How searches treat case, as the options say.
    fn case(&self) -> Case;

    /// What the bar says when no item matches `query`.
    fn missed(&self, query: &str) -> String;

    /// Says `said` in the bar till the next key.
    fn say(&mut self, said: String);

    /// Notes that a search failed, as Vim beeps, stopping a macro.
    fn fail(&mut self);

    /// Whether a search failed since this was last asked.
    fn take_failed(&mut self) -> bool;
}

/// A list as the shared search drives it (see [`super::search`]): it finds
/// the `count`th item that matches from an item, round the list's ends,
/// looking at each item once however big the count, and goes there.
pub(crate) struct Searching<'a, L: ?Sized>(pub(crate) &'a mut L);

impl<L: Searched + ?Sized> Restores for Searching<'_, L> {
    type Kinds = Items;

    fn search(&mut self) -> &mut Search<Items> {
        self.0.search_mut()
    }

    fn keep(&self) -> (usize, usize) {
        (self.0.cursor(), self.0.top())
    }

    fn put_back(&mut self, &mut (cursor, top): &mut (usize, usize), keep_view: bool) {
        let final_item = self.0.len().saturating_sub(1);
        self.0.set_cursor(cursor.min(final_item));
        if !keep_view {
            self.0.set_top(top.min(final_item));
        }
    }
}

impl<L: Searched + ?Sized> Searcher for Searching<'_, L> {
    fn compile(&self, last: &Last) -> Result<Pattern, String> {
        let case = self.0.case();
        let matching = if last.word {
            Matching::Always(case)
        } else {
            Matching::Typed(case)
        };
        Pattern::new(&last.query, matching, "", Words::NONE)
    }

    fn find(&mut self, find: Find<'_, Items>) -> Finding<usize> {
        let from = find
            .preview
            .map_or(self.0.cursor(), |&mut (cursor, _)| cursor);
        let list = &*self.0;
        let found = round(list.len(), from, find.count, find.backward, |item| {
            list.matches(item, find.lit)
        });
        match found {
            Some(at) => Finding::Found {
                at,
                wrapped: if find.backward {
                    at >= from
                } else {
                    at <= from
                },
            },
            None => Finding::Missed(list.missed(&find.last.query)),
        }
    }

    fn go(&mut self, found: usize) {
        self.0.set_cursor(found);
        follow(self.0);
    }

    fn say(&mut self, said: String) {
        self.0.say(said);
    }

    fn fail(&mut self) {
        self.0.fail();
    }
}

/// Takes `stroke` for `list`'s search, before its other keys: every key
/// while one's typed, and Esc with nothing typed before it, which puts
/// out what the last search lit, as `:nohlsearch` does. Returns whether it
/// took it.
pub(crate) fn search_stroke(list: &mut impl Searched, stroke: Stroke) -> bool {
    if list.search().typing() {
        super::search::key(&mut Searching(list), stroke);
        return true;
    }
    if stroke == Stroke::Escape && !list.keys().pending() && list.search().lit().is_some() {
        list.search_mut().unlight();
        return true;
    }
    false
}

/// The strokes `press` makes in `list`: a prompt's while a search is typed,
/// as in every pane's bar, so Alt and a key types it and Ctrl+Backspace
/// deletes a word; else a list's (see [`super::plain_strokes`]).
pub(crate) fn strokes(press: &crate::input::KeyPress<'_>, list: &impl Searched) -> Vec<Stroke> {
    if list.search().typing() {
        super::prompt_strokes(press)
    } else {
        super::plain_strokes(press, super::ListKeys::prompting(list))
    }
}

/// `/` and `?`, and `n` and `N`, in `list`, as `command` asks, if it's
/// one of them, with its count. Returns whether it was.
pub(crate) fn search_command<O>(list: &mut impl Searched, command: &Command<O>) -> bool {
    match command.action {
        Action::Search { backward } => {
            super::search::start(&mut Searching(list), backward, command.count);
        }
        Action::SearchAgain { reverse } => {
            super::search::again(&mut Searching(list), reverse, command.times());
        }
        _ => return false,
    }
    true
}

/// The matches lit in an item's text: each one's bytes, and whether it's
/// the one the search went to.
pub(crate) type LitIn = Vec<(Range<usize>, bool)>;

/// Where in `text`, the text of an item in view, the last search's
/// matches are lit, from `list`'s search: each match's bytes, and whether
/// it's the one the search went to, `found` saying whether this item is
/// that one. With `hlsearch`, every match, one after another, no more than
/// [`MOST_LIT`]; without it, only the found item's first.
///
/// [`MOST_LIT`]: crate::hlsearch::MOST_LIT
pub(crate) fn lit_in(search: &Search<Items>, text: &str, found: bool) -> LitIn {
    let Some(pattern) = search.lit() else {
        return Vec::new();
    };
    if !search.every {
        let first = (pattern.find_at(text, 0)).filter(|_| found);
        return first
            .map(|first| (first.start..first.end, true))
            .into_iter()
            .collect();
    }
    let spans = [(0, 0..text.len())];
    let mut lit: LitIn = (pattern
        .matches_in(text, spans, crate::hlsearch::MOST_LIT)
        .into_iter())
    .map(|range| (range, false))
    .collect();
    // The first in the item the search went to is the one found.
    if found && let Some(first) = lit.first_mut() {
        first.1 = true;
    }
    lit
}

/// Puts `text` at the end of what's typed in a list's bar, `typed`: its
/// line breaks as spaces, and nothing else that isn't printed.
pub(crate) fn paste_into(typed: &mut String, text: &str) {
    typed.extend(
        text.replace("\r\n", "\n")
            .chars()
            .map(|ch| {
                if matches!(ch, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    ch
                }
            })
            .filter(|ch| !ch.is_control()),
    );
}

/// The `count`th of `len` items that `matches` after item `at`, or
/// before it if `backward`, round from the end to the start, `at` itself
/// last; `None` if none does. Looks at each item once, however big the
/// count, as going round as often as it says comes back where it started.
pub(crate) fn round(
    len: usize,
    at: usize,
    count: usize,
    backward: bool,
    matches: impl Fn(usize) -> bool,
) -> Option<usize> {
    let found: Vec<usize> = (0..len).filter(|&item| matches(item)).collect();
    let total = found.len();
    if total == 0 {
        return None;
    }
    let steps = (count.max(1) - 1) % total + 1;
    let index = if backward {
        // The matches before `at`, the nearest last.
        let before = found.partition_point(|&item| item < at);
        (before + total - steps) % total
    } else {
        let through = found.partition_point(|&item| item <= at);
        (through + steps - 1) % total
    };
    Some(found[index])
}

/// The moves of its own every list has besides Vim's, as pickers have
/// them: Home and End to the first item and the last, Ctrl+N and Ctrl+P
/// to the next and the one before.
pub(crate) fn own_move(stroke: Stroke) -> Option<Motion> {
    Some(match stroke {
        Stroke::Home => Motion::First,
        Stroke::End => Motion::Last,
        Stroke::Char {
            ch: 'n',
            ctrl: true,
        } => Motion::Down(1),
        Stroke::Char {
            ch: 'p',
            ctrl: true,
        } => Motion::Up(1),
        _ => return None,
    })
}

/// What [`own_move`]'s `motion` does, as a list's keys' help says it.
pub(crate) fn describe_move(motion: Motion) -> String {
    match motion {
        Motion::First => "First item",
        Motion::Last => "Last item",
        Motion::Up(_) => "Previous item",
        _ => "Next item",
    }
    .into()
}

/// Does [`own_move`]'s `motion`, `count` typed, in `list`.
pub(crate) fn go_own(
    list: &mut impl ListRows,
    paging: &mut Paging,
    motion: Motion,
    count: Option<usize>,
) {
    // Home and End go to the ends whatever the count, as they do in
    // pickers.
    let count = count.filter(|_| matches!(motion, Motion::Up(_) | Motion::Down(_)));
    let command: Command<()> = Command {
        count,
        action: Action::Move(motion),
        register: None,
    };
    go(list, paging, &command);
}

/// What a list's bar says, `label`, with the keys of a command half typed
/// before it, as Vim's `showcmd` shows them.
pub(crate) fn showing(keys: &Keys, label: String) -> String {
    match keys.shown() {
        Some(keys) => format!("{keys}  {label}"),
        None => label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list of `len` items, each a row, in a view `height` rows tall.
    struct Rows {
        len: usize,
        height: usize,
        cursor: usize,
        top: usize,
    }

    impl ListRows for Rows {
        fn len(&self) -> usize {
            self.len
        }
        fn cursor(&self) -> usize {
            self.cursor
        }
        fn set_cursor(&mut self, item: usize) {
            self.cursor = item;
        }
        fn top(&self) -> usize {
            self.top
        }
        fn set_top(&mut self, item: usize) {
            self.top = item.min(self.len.saturating_sub(self.height));
        }
        fn shown(&self) -> Range<usize> {
            self.top..(self.top + self.height).min(self.len).max(self.top + 1)
        }
    }

    fn rows(cursor: usize, top: usize) -> Rows {
        Rows {
            len: 100,
            height: 10,
            cursor,
            top,
        }
    }

    fn command(count: Option<usize>, action: Action<()>) -> Command<()> {
        Command {
            count,
            action,
            register: None,
        }
    }

    fn moved(list: &mut Rows, count: Option<usize>, action: Action<()>) -> (usize, usize) {
        assert!(go(list, &mut Paging::default(), &command(count, action)));
        (list.cursor, list.top)
    }

    #[test]
    fn lines_go_by_counts_and_numbers_as_vims_do() {
        let down = Action::Move(Motion::Down(1));
        assert_eq!(moved(&mut rows(0, 0), None, down), (1, 0));
        assert_eq!(moved(&mut rows(0, 0), Some(5), down), (5, 0));
        assert_eq!(moved(&mut rows(98, 90), Some(99_999_999), down), (99, 90));
        let up = Action::Move(Motion::Up(1));
        assert_eq!(moved(&mut rows(3, 0), Some(9), up), (0, 0));
        assert_eq!(
            moved(&mut rows(50, 45), None, Action::Move(Motion::First)),
            (0, 45)
        );
        assert_eq!(
            moved(&mut rows(0, 0), Some(7), Action::Move(Motion::First)),
            (6, 0)
        );
        assert_eq!(
            moved(&mut rows(0, 0), None, Action::Move(Motion::Last)),
            (99, 0)
        );
        assert_eq!(
            moved(&mut rows(0, 0), Some(7), Action::Move(Motion::Last)),
            (6, 0)
        );
        assert_eq!(
            moved(&mut rows(0, 0), Some(500), Action::Move(Motion::Last)),
            (99, 0)
        );
    }

    #[test]
    fn h_m_and_l_go_within_the_view() {
        let view = |motion| Action::Move(motion);
        assert_eq!(
            moved(&mut rows(25, 20), None, view(Motion::ViewTop)),
            (20, 20)
        );
        assert_eq!(
            moved(&mut rows(25, 20), Some(3), view(Motion::ViewTop)),
            (22, 20)
        );
        assert_eq!(
            moved(&mut rows(25, 20), Some(30), view(Motion::ViewTop)),
            (29, 20)
        );
        assert_eq!(
            moved(&mut rows(21, 20), None, view(Motion::ViewBottom)),
            (29, 20)
        );
        assert_eq!(
            moved(&mut rows(21, 20), Some(4), view(Motion::ViewBottom)),
            (26, 20)
        );
        assert_eq!(
            moved(&mut rows(21, 20), None, view(Motion::ViewMiddle)),
            (24, 20)
        );
        // Fewer items than the view holds: the middle of those there are.
        let mut short = Rows {
            len: 5,
            height: 10,
            cursor: 0,
            top: 0,
        };
        assert_eq!(moved(&mut short, None, view(Motion::ViewMiddle)), (2, 0));
        assert_eq!(moved(&mut short, None, view(Motion::ViewBottom)), (4, 0));
    }

    #[test]
    fn scrolling_keys_go_as_far_as_in_every_pane() {
        let scroll = |scroll| Action::Scroll(scroll);
        // Ctrl+E: the view a line, the cursor kept in it.
        let line = scroll(Scroll::Line { down: true });
        assert_eq!(moved(&mut rows(20, 20), None, line), (21, 21));
        assert_eq!(moved(&mut rows(25, 20), Some(3), line), (25, 23));
        let back = scroll(Scroll::Line { down: false });
        assert_eq!(moved(&mut rows(29, 20), None, back), (28, 19));
        // Ctrl+D: view and cursor half the view; at the end, the cursor
        // goes on alone.
        let half = scroll(Scroll::HalfPage { down: true });
        assert_eq!(moved(&mut rows(22, 20), None, half), (27, 25));
        assert_eq!(moved(&mut rows(91, 90), None, half), (96, 90));
        // A view not yet moved to the cursor, as keys typed in one go
        // leave it, is first.
        assert_eq!(moved(&mut rows(0, 2), None, half), (5, 5));
        let half_up = scroll(Scroll::HalfPage { down: false });
        assert_eq!(moved(&mut rows(27, 25), None, half_up), (22, 20));
        assert_eq!(moved(&mut rows(4, 0), None, half_up), (0, 0));
        // Ctrl+F: a page, the view less two, the cursor to its top; at the
        // end, to the last item.
        let page = scroll(Scroll::Page { down: true });
        assert_eq!(moved(&mut rows(5, 0), None, page), (8, 8));
        assert_eq!(moved(&mut rows(95, 90), None, page), (99, 90));
        let page_up = scroll(Scroll::Page { down: false });
        assert_eq!(moved(&mut rows(20, 20), None, page_up), (21, 12));
        assert_eq!(moved(&mut rows(5, 0), None, page_up), (0, 0));
    }

    #[test]
    fn the_cursor_s_item_goes_to_the_view_s_top_middle_or_bottom() {
        let at = |at| Action::View(at);
        assert_eq!(moved(&mut rows(50, 45), None, at(ViewAt::Top)), (50, 50));
        assert_eq!(moved(&mut rows(50, 45), None, at(ViewAt::Middle)), (50, 46));
        assert_eq!(moved(&mut rows(50, 45), None, at(ViewAt::Bottom)), (50, 41));
        assert_eq!(moved(&mut rows(50, 45), Some(3), at(ViewAt::Top)), (2, 2));
        assert_eq!(moved(&mut rows(3, 0), None, at(ViewAt::Bottom)), (3, 0));
    }

    #[test]
    fn a_list_takes_only_the_moves_it_can_do() {
        assert!(moves(&command(None, Action::Move(Motion::Down(1)))));
        assert!(moves(&command(
            None,
            Action::Scroll(Scroll::Page { down: true })
        )));
        assert!(!moves(&command(None, Action::Move(Motion::WordStart))));
        assert!(!moves(&command(None, Action::Undo)));
        let mut list = rows(3, 0);
        let word = command(None, Action::Move(Motion::WordStart));
        assert!(!go(&mut list, &mut Paging::default(), &word));
        assert_eq!((list.cursor, list.top), (3, 0));
    }

    /// A list whose view, laid out before it shrank, starts past its end.
    struct Stale;

    impl ListRows for Stale {
        fn len(&self) -> usize {
            3
        }
        fn cursor(&self) -> usize {
            2
        }
        fn set_cursor(&mut self, item: usize) {
            assert!(item < 3, "{item}");
        }
        fn top(&self) -> usize {
            8
        }
        fn set_top(&mut self, _item: usize) {}
        fn shown(&self) -> Range<usize> {
            8..12
        }
    }

    #[test]
    fn a_view_left_past_the_lists_end_never_puts_the_cursor_past_it() {
        for scroll in [
            Scroll::Line { down: true },
            Scroll::HalfPage { down: true },
            Scroll::HalfPage { down: false },
            Scroll::Page { down: true },
            Scroll::Page { down: false },
        ] {
            go(
                &mut Stale,
                &mut Paging::default(),
                &command(None, Action::Scroll(scroll)),
            );
        }
        go(
            &mut Stale,
            &mut Paging::default(),
            &command(None, Action::Move(Motion::ViewTop)),
        );
        wheel(&mut Stale, 2);
    }

    /// [`Rows`] of names, searched.
    #[derive(Default)]
    struct Named {
        names: Vec<&'static str>,
        cursor: usize,
        top: usize,
        keys: Keys,
        search: Search<Items>,
        said: Option<String>,
        failed: bool,
    }

    impl ListRows for Named {
        fn len(&self) -> usize {
            self.names.len()
        }
        fn cursor(&self) -> usize {
            self.cursor
        }
        fn set_cursor(&mut self, item: usize) {
            self.cursor = item;
        }
        fn top(&self) -> usize {
            self.top
        }
        fn set_top(&mut self, item: usize) {
            self.top = item;
        }
        fn shown(&self) -> Range<usize> {
            self.top..(self.top + 3).min(self.names.len())
        }
    }

    impl super::super::ListKeys for Named {
        fn keys(&self) -> &Keys {
            &self.keys
        }
        fn keys_mut(&mut self) -> &mut Keys {
            &mut self.keys
        }
        fn prompting(&self) -> bool {
            self.search.typing()
        }
    }

    impl Searched for Named {
        fn search(&self) -> &Search<Items> {
            &self.search
        }
        fn search_mut(&mut self) -> &mut Search<Items> {
            &mut self.search
        }
        fn matches(&self, item: usize, pattern: &Pattern) -> bool {
            pattern.find_at(self.names[item], 0).is_some()
        }
        fn case(&self) -> Case {
            Case::default()
        }
        fn missed(&self, query: &str) -> String {
            format!("no name matches {query}")
        }
        fn say(&mut self, said: String) {
            self.said = Some(said);
        }
        fn fail(&mut self) {
            self.failed = true;
        }
        fn take_failed(&mut self) -> bool {
            std::mem::take(&mut self.failed)
        }
    }

    fn named(names: &[&'static str]) -> Named {
        Named {
            names: names.to_vec(),
            ..Named::default()
        }
    }

    fn typed(list: &mut Named, keys: &str) {
        for ch in keys.chars() {
            assert!(search_stroke(list, Stroke::char(ch)));
        }
    }

    #[test]
    fn a_search_shows_where_it_goes_as_its_typed_and_counts() {
        let mut list = named(&["a", "bx", "c", "dx", "e", "fx", "g"]);
        search_command(
            &mut list,
            &command(Some(2), Action::Search { backward: false }),
        );
        assert_eq!(list.search.label().as_deref(), Some("/▏"));
        typed(&mut list, "x");
        super::super::search::preview(&mut Searching(&mut list));
        assert_eq!(list.cursor, 3);
        // Lit, the item found's first match the one found.
        assert_eq!(
            lit_in(&list.search, "xox", true),
            [(0..1, true), (2..3, false)]
        );
        assert_eq!(
            lit_in(&list.search, "xox", false),
            [(0..1, false), (2..3, false)]
        );
        // Esc puts it back.
        assert!(search_stroke(&mut list, Stroke::Escape));
        assert_eq!((list.cursor, list.search.lit().is_none()), (0, true));
        // Enter goes there.
        search_command(
            &mut list,
            &command(Some(2), Action::Search { backward: false }),
        );
        typed(&mut list, "x");
        search_stroke(&mut list, Stroke::Enter);
        assert_eq!(list.cursor, 3);
        assert!(!list.search.typing());
        // `N` back, and round.
        search_command(
            &mut list,
            &command(Some(2), Action::SearchAgain { reverse: true }),
        );
        assert_eq!(
            (list.cursor, list.said.as_deref()),
            (5, Some(super::super::WRAPPED_TO_BOTTOM))
        );
        // Esc with nothing typed puts out what's lit.
        assert!(search_stroke(&mut list, Stroke::Escape));
        assert!(list.search.lit().is_none());
        assert!(!search_stroke(&mut list, Stroke::Escape));
        // A search that finds nothing fails.
        search_command(
            &mut list,
            &command(None, Action::Search { backward: false }),
        );
        typed(&mut list, "zz");
        search_stroke(&mut list, Stroke::Enter);
        assert_eq!(
            (list.failed, list.said.as_deref()),
            (true, Some("no name matches zz"))
        );
    }

    #[test]
    fn without_hlsearch_only_the_match_found_is_lit() {
        let mut list = named(&["ax", "bx"]);
        list.search.every = false;
        search_command(
            &mut list,
            &command(None, Action::Search { backward: false }),
        );
        typed(&mut list, "x");
        search_stroke(&mut list, Stroke::Enter);
        assert_eq!(lit_in(&list.search, "bx", true), [(1..2, true)]);
        assert!(lit_in(&list.search, "ax", false).is_empty());
    }

    #[test]
    fn a_paste_is_one_line_whatever_breaks_it() {
        let mut typed = String::from(">");
        paste_into(&mut typed, "a\r\nb\rc\nd\te\u{7}");
        assert_eq!(typed, ">a b c d e");
    }

    #[test]
    fn searching_again_goes_round_counts_without_walking_them() {
        let matches = |item: usize| [2, 5, 9].contains(&item);
        assert_eq!(round(12, 0, 1, false, matches), Some(2));
        assert_eq!(round(12, 2, 1, false, matches), Some(5));
        assert_eq!(round(12, 2, 2, false, matches), Some(9));
        assert_eq!(round(12, 9, 1, false, matches), Some(2));
        assert_eq!(round(12, 5, 1, true, matches), Some(2));
        assert_eq!(round(12, 2, 1, true, matches), Some(9));
        assert_eq!(round(12, 6, 3, true, matches), Some(9));
        // A count as big as there is: once round is the same as none.
        assert_eq!(round(12, 2, 99_999_999, false, matches), Some(2));
        assert_eq!(round(12, 2, 3, false, matches), Some(2));
        assert_eq!(round(12, 0, 1, false, |_| false), None);
    }
}
