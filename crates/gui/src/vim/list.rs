//! Panes of lists, on the grammar every pane reads keys with: an
//! explorer's files, a board's entries, a review's lines. The cursor is on
//! an item, not a character, so of Vim's motions they take those that go
//! between lines, `j` `k` `gg` `G` `H` `M` `L`, with counts, and the
//! scrolling keys, Ctrl+E Ctrl+Y Ctrl+D Ctrl+U Ctrl+F and the pages, and
//! `zt` `zz` `zb`, all done here once for every list ([`go`]); and `/` `?`
//! `n` `N`, searching the items shown ([`ListSearch`]). The rest of their
//! keys are their own, through their [`Dialect`](super::Dialect)'s hooks.

use std::ops::Range;

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
fn follow(list: &mut impl ListRows) {
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

/// What a list's search is: being typed in the bar, and the last one
/// done, for `n` and `N`, and whether each goes backward.
#[derive(Debug, Default)]
pub(crate) struct ListSearch {
    typing: Option<(String, bool)>,
    last: Option<(String, bool)>,
}

/// What a key typed into a list's search did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Typed {
    /// It was typed, or the search was given up.
    Edited,
    /// Enter: the search is the last now, to go to with
    /// [`ListSearch::again`].
    Done,
}

impl ListSearch {
    /// `/`, or `?` if `backward`: starts typing a search.
    pub(crate) fn start(&mut self, backward: bool) {
        self.typing = Some((String::new(), backward));
    }

    /// Whether a search is being typed, which takes every key.
    pub(crate) fn typing(&self) -> bool {
        self.typing.is_some()
    }

    /// What the bar says while it's typed: `/` or `?`, and what's typed.
    pub(crate) fn label(&self) -> Option<String> {
        let (query, backward) = self.typing.as_ref()?;
        Some(format!("{}{query}▏", if *backward { '?' } else { '/' }))
    }

    /// What's typed so far, for its keys' help.
    pub(crate) fn typed(&self) -> Option<&str> {
        self.typing.as_ref().map(|(query, _)| query.as_str())
    }

    /// Takes `stroke` for the search being typed. Enter makes it the last,
    /// or with nothing typed, the last again the way this one goes.
    pub(crate) fn key(&mut self, stroke: Stroke) -> Typed {
        let Some((query, _)) = &mut self.typing else {
            return Typed::Edited;
        };
        match crate::cmdline::simple::key(query, stroke) {
            crate::cmdline::simple::Outcome::Accept => {
                let Some((query, backward)) = self.typing.take() else {
                    return Typed::Edited;
                };
                if query.is_empty() {
                    match &mut self.last {
                        Some((_, last)) => *last = backward,
                        None => return Typed::Edited,
                    }
                } else {
                    self.last = Some((query, backward));
                }
                Typed::Done
            }
            crate::cmdline::simple::Outcome::Cancel => {
                self.typing = None;
                Typed::Edited
            }
            _ => Typed::Edited,
        }
    }

    /// Pastes `text` into the search being typed, if one is, its line
    /// breaks as spaces, as the bar is one line. Returns whether one was.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        let Some((query, _)) = &mut self.typing else {
            return false;
        };
        paste_into(query, text);
        true
    }

    /// Forgets the search being typed, as leaving the pane does.
    pub(crate) fn give_up(&mut self) {
        self.typing = None;
    }

    /// The last search, as typed.
    pub(crate) fn query(&self) -> Option<&str> {
        self.last.as_ref().map(|(query, _)| query.as_str())
    }

    /// The last search as a pattern, with case as `case` says, as the
    /// editor's typed patterns are: `None` if there's been none, `Err`
    /// saying why if it can't be matched.
    pub(crate) fn pattern(&self, case: Case) -> Option<Result<Pattern, String>> {
        let query = self.query()?;
        Some(Pattern::new(query, Matching::Typed(case), "", Words::NONE))
    }

    /// `n`, or `N` if `reverse`: the `count`th of `len` items that
    /// `matches` after item `at`, or before it if the last search went
    /// backward, or the other way if `reverse`, round from the end to the
    /// start; `None` if none does. Looks at each item once, however big
    /// the count.
    pub(crate) fn again(
        &self,
        reverse: bool,
        count: usize,
        len: usize,
        at: usize,
        matches: impl Fn(usize) -> bool,
    ) -> Option<usize> {
        let backward = self.last.as_ref().is_some_and(|(_, backward)| *backward) != reverse;
        round(len, at, count, backward, matches)
    }
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

    fn searched(text: &str) -> ListSearch {
        let mut search = ListSearch::default();
        search.start(text.starts_with('?'));
        for ch in text[1..].chars() {
            assert_eq!(search.key(Stroke::char(ch)), Typed::Edited);
        }
        search
    }

    #[test]
    fn a_search_typed_is_the_last_once_entered_and_the_last_again_if_empty() {
        let mut search = searched("/ab");
        assert!(search.typing());
        assert_eq!(search.label().as_deref(), Some("/ab▏"));
        assert_eq!(search.query(), None);
        assert_eq!(search.key(Stroke::Enter), Typed::Done);
        assert!(!search.typing());
        assert_eq!(search.query(), Some("ab"));
        // `?` and Enter: the last again, backward.
        search.start(true);
        assert_eq!(search.key(Stroke::Enter), Typed::Done);
        assert_eq!(search.query(), Some("ab"));
        assert_eq!(search.again(false, 1, 10, 5, |item| item == 2), Some(2));
        // Esc gives up, keeping the last.
        search.start(false);
        search.key(Stroke::char('x'));
        assert_eq!(search.key(Stroke::Escape), Typed::Edited);
        assert_eq!(search.query(), Some("ab"));
        // Nothing to search again with none before.
        let mut fresh = ListSearch::default();
        fresh.start(false);
        assert_eq!(fresh.key(Stroke::Enter), Typed::Edited);
    }

    #[test]
    fn a_paste_is_one_line_whatever_breaks_it() {
        let mut typed = String::from(">");
        paste_into(&mut typed, "a\r\nb\rc\nd\te\u{7}");
        assert_eq!(typed, ">a b c d e");
    }

    #[test]
    fn searching_again_goes_round_counts_without_walking_them() {
        let mut search = searched("/x");
        search.key(Stroke::Enter);
        let matches = |item: usize| [2, 5, 9].contains(&item);
        assert_eq!(search.again(false, 1, 12, 0, matches), Some(2));
        assert_eq!(search.again(false, 1, 12, 2, matches), Some(5));
        assert_eq!(search.again(false, 2, 12, 2, matches), Some(9));
        assert_eq!(search.again(false, 1, 12, 9, matches), Some(2));
        assert_eq!(search.again(true, 1, 12, 5, matches), Some(2));
        assert_eq!(search.again(true, 1, 12, 2, matches), Some(9));
        assert_eq!(search.again(true, 3, 12, 6, matches), Some(9));
        // A count as big as there is: once round is the same as none.
        assert_eq!(search.again(false, 99_999_999, 12, 2, matches), Some(2));
        assert_eq!(search.again(false, 3, 12, 2, matches), Some(2));
        assert_eq!(search.again(false, 1, 12, 0, |_| false), None);
    }
}
