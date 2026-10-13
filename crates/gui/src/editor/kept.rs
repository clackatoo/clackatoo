//! Lines laid out, and the rows they wrap onto, kept from one key and
//! frame to the next, and through edits.
//!
//! Motions ask for one line many times, and every key and frame for the
//! lines in view, which can be long: a minified file's line of megabytes
//! takes tens of milliseconds to lay out. So an editor keeps what it's
//! laid out, and an edit doesn't throw it all away: it forgets only the
//! lines it touched, moves those after it up or down by the lines it
//! added or took away, and of the line it was made in, keeps what's
//! before it, which is laid out as it was. Typing at the end of a long
//! line lays out only the end.
//!
//! The edits come from the buffer's copy, which notes what each did to
//! the text's lines in a [`LineLog`], as it makes it.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use clackatoo_protocol::BufferId;
use clackatoo_text::{Edit, Rope};

use super::text::Laid;
use super::wrap::{self, Wrap};
use crate::motion::ScreenRow;

/// What one change of an edit did to a text's lines: on line `line`, from
/// its byte `byte`, it took away `removed` bytes holding `joined` line
/// breaks, and put `inserted` bytes holding `split` in their place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineChange {
    line: u64,
    byte: usize,
    removed: usize,
    inserted: usize,
    joined: u64,
    split: u64,
    /// It took away and put in whole lines, as `dd`, `p` of lines and `O`
    /// do: the line it ends before is as it was.
    whole: bool,
}

/// An edit of more changes than this isn't followed: what's laid out is
/// done again, as a `:%s` of thousands of lines would mostly need anyway.
const MOST_CHANGES: usize = 256;

impl LineChange {
    /// The changes `edit` makes to `before`, the text it's made to, in
    /// order, or `None` if it makes too many to follow.
    pub(crate) fn of(edit: &Edit, before: &Rope) -> Option<Box<[Self]>> {
        let breaks = |text: &str| text.bytes().filter(|&byte| byte == b'\n').count() as u64;
        let lines = |text: &str| text.is_empty() || text.ends_with('\n');
        let mut changes = Vec::new();
        for (at, removed, inserted) in edit.changes() {
            if changes.len() == MOST_CHANGES || at > before.len_bytes() {
                return None;
            }
            let line = before.byte_to_line(at);
            let byte = at - before.line_to_byte(line);
            changes.push(Self {
                line: line as u64,
                byte,
                removed: removed.len(),
                inserted: inserted.len(),
                joined: breaks(removed),
                split: breaks(inserted),
                whole: byte == 0 && lines(removed) && lines(inserted),
            });
        }
        Some(changes.into())
    }

    /// Whether it stays in its line, adding and taking away no line
    /// breaks.
    fn in_line(&self) -> bool {
        self.joined == 0 && self.split == 0
    }

    /// The line after the last it changes or takes away.
    fn end(&self) -> u64 {
        self.line + self.joined + u64::from(!self.whole)
    }
}

/// What the last few edits of a buffer's text did to its lines, by the
/// revision each made, so what's laid out of it can follow them.
#[derive(Debug, Default)]
pub(crate) struct LineLog {
    /// The revision the first edit noted made.
    first: u64,
    /// Each edit's changes, oldest first, or `None` for one too big to
    /// follow.
    edits: VecDeque<Option<Box<[LineChange]>>>,
    /// The changes noted, in all.
    changes: usize,
}

/// The most edits a [`LineLog`] keeps: a pane that falls further behind, as
/// one not drawn while a macro runs, lays its lines out again.
const MOST_EDITS: usize = 64;

/// The most changes a [`LineLog`] keeps, in all.
const MOST_LOGGED: usize = 1024;

impl LineLog {
    /// Notes `changes`, which made revision `revision`, as
    /// [`LineChange::of`] found them.
    pub(crate) fn push(&mut self, revision: u64, changes: Option<Box<[LineChange]>>) {
        if revision != self.first + self.edits.len() as u64 {
            // Edits were made that weren't noted.
            self.edits.clear();
            self.changes = 0;
            self.first = revision;
        }
        self.changes += changes.as_ref().map_or(0, |changes| changes.len());
        self.edits.push_back(changes);
        while self.edits.len() > MOST_EDITS || self.changes > MOST_LOGGED {
            let Some(oldest) = self.edits.pop_front() else {
                break;
            };
            self.changes -= oldest.map_or(0, |changes| changes.len());
            self.first += 1;
        }
    }

    /// Forgets every edit: the text was replaced, making revision
    /// `revision`, and nothing laid out before can follow it.
    pub(crate) fn clear(&mut self, revision: u64) {
        self.edits.clear();
        self.changes = 0;
        self.first = revision + 1;
    }

    /// The changes each edit after revision `from` made, as far as
    /// revision `to`, in order, if every one's noted and can be followed.
    fn since(&self, from: u64, to: u64) -> Option<Vec<&[LineChange]>> {
        let start = usize::try_from((from + 1).checked_sub(self.first)?).ok()?;
        let end = usize::try_from((to + 1).checked_sub(self.first)?).ok()?;
        if end > self.edits.len() || start > end {
            return None;
        }
        (self.edits.range(start..end))
            .map(|changes| changes.as_deref())
            .collect()
    }
}

/// Lines laid out, and the rows they wrap onto, kept for one editor. A
/// clone shares them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Kept(Rc<RefCell<KeptLines>>);

#[derive(Debug, Default)]
struct KeptLines {
    /// What they're laid out for: the buffer, and the tab stop.
    key: Option<(BufferId, usize)>,
    /// The revision of the buffer's text they're laid out for.
    revision: u64,
    /// How lines wrap, which their rows are for.
    wrap: Option<Wrap>,
    lines: BTreeMap<u64, KeptLine>,
    /// About how many bytes they take, in all.
    size: usize,
}

/// What's kept of one line.
#[derive(Debug)]
struct KeptLine {
    laid: Rc<Laid>,
    /// Where its text has changed since it was laid out, if it has.
    changed: Option<Changed>,
    /// Its rows, without and with room after its end for the cursor
    /// typing there, if they've been asked for.
    rows: [Option<Rows>; 2],
    /// About how many bytes it takes.
    size: usize,
}

/// Where a line's text has changed since it was laid out: from byte
/// `from` on, but not in its last `tail` bytes. It's `len` bytes long now.
#[derive(Clone, Copy, Debug)]
struct Changed {
    from: usize,
    tail: usize,
    len: usize,
}

/// A line's rows, and the cell they're out of date from, if they are.
#[derive(Debug)]
struct Rows {
    rows: Rc<[ScreenRow]>,
    stale: Option<usize>,
}

impl KeptLine {
    fn new(laid: Rc<Laid>) -> Self {
        let mut line = Self {
            laid,
            changed: None,
            rows: [None, None],
            size: 0,
        };
        line.measure();
        line
    }

    /// Works out about how many bytes it takes.
    fn measure(&mut self) {
        let rows = (self.rows.iter().flatten())
            .map(|rows| rows.rows.len() * size_of::<ScreenRow>())
            .sum::<usize>();
        self.size = self.laid.size() + rows;
    }

    /// Notes `changes`, which all start in the line, in order.
    fn edited(&mut self, changes: &[LineChange]) {
        let (Some(first), Some(last)) = (changes.first(), changes.last()) else {
            return;
        };
        let was = self.changed.unwrap_or(Changed {
            from: usize::MAX,
            tail: self.laid.text().len(),
            len: self.laid.text().len(),
        });
        // The line goes on as another did, or into another, past a change
        // that adds or takes away a line break: none of its end is kept.
        let in_line = changes.iter().all(LineChange::in_line);
        let len = (changes.iter()).fold(was.len, |len, change| {
            (len + change.inserted).saturating_sub(change.removed)
        });
        self.changed = Some(Changed {
            from: was.from.min(first.byte),
            tail: if in_line {
                was.tail
                    .min(was.len.saturating_sub(last.byte + last.removed))
            } else {
                0
            },
            len: if in_line { len } else { 0 },
        });
    }
}

/// The most lines a [`Kept`] keeps: more, as when scrolling a long way,
/// and it forgets those furthest from the last asked for.
const KEPT: usize = 1024;

/// About the most bytes a [`Kept`] keeps, as it does, for each pane, for as
/// long as the pane's open. A line that takes more on its own, as a long
/// minified one can, is kept alone.
const KEPT_BYTES: usize = 16 << 20;

impl KeptLines {
    /// Keeps `kept` as line `line`'s, then forgets the lines furthest from
    /// it while there's too much kept. Line `line`, the one asked for
    /// last, near the view, stays.
    fn keep(&mut self, line: u64, kept: KeptLine) {
        self.size += kept.size;
        if let Some(old) = self.lines.insert(line, kept) {
            self.size -= old.size;
        }
        self.trim(line);
    }

    /// Forgets the lines furthest from line `line` while there's too much
    /// kept.
    fn trim(&mut self, line: u64) {
        while self.size > KEPT_BYTES || self.lines.len() > KEPT {
            let first = self.lines.first_key_value().map(|(&first, _)| first);
            let last = self.lines.last_key_value().map(|(&last, _)| last);
            let furthest = [first, last]
                .into_iter()
                .flatten()
                .filter(|&kept| kept != line)
                .max_by_key(|&kept| kept.abs_diff(line));
            let Some(furthest) = furthest else {
                break;
            };
            self.forget(furthest);
        }
    }

    /// Takes line `line`'s out, if it's kept.
    fn forget(&mut self, line: u64) -> Option<KeptLine> {
        let kept = self.lines.remove(&line)?;
        self.size -= kept.size;
        Some(kept)
    }

    /// Forgets everything kept.
    fn clear(&mut self) {
        self.lines.clear();
        self.size = 0;
    }

    /// Follows an edit's `changes`: the lines they're made in have changed
    /// from where they start, the lines they take away go, and the lines
    /// after them move by the line breaks they add and take away.
    fn follow(&mut self, changes: &[LineChange]) {
        let Some(first) = changes.first() else {
            return;
        };
        if changes.iter().all(LineChange::in_line) {
            // No line moves.
            for group in changes.chunk_by(|a, b| a.line == b.line) {
                if let Some(kept) = self.lines.get_mut(&group[0].line) {
                    kept.edited(group);
                }
            }
            return;
        }
        // How far the lines after each change move.
        let mut moves = Vec::with_capacity(changes.len() + 1);
        moves.push(0_i64);
        for change in changes {
            let before = moves.last().copied().unwrap_or(0);
            #[allow(clippy::cast_possible_wrap)]
            moves.push(before + change.split as i64 - change.joined as i64);
        }
        let after = self.lines.split_off(&first.line);
        for (line, mut kept) in after {
            // The first change to end after the line.
            let at = changes.partition_point(|change| change.end() <= line);
            match changes.get(at) {
                // Made from its start.
                Some(change) if change.line == line && !change.whole => {
                    let made = changes[at..].partition_point(|change| change.line == line);
                    kept.edited(&changes[at..at + made]);
                }
                // Taken away.
                Some(change) if change.line <= line => {
                    self.size -= kept.size;
                    continue;
                }
                _ => {}
            }
            #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
            let to = (line as i64 + moves[at]) as u64;
            self.lines.insert(to, kept);
        }
    }
}

impl Kept {
    /// Makes it hold what's laid out of revision `revision` of `buffer`'s
    /// text, with tabs stopping at multiples of `tabstop` and lines
    /// wrapping as `wrap` says: what it held for an earlier revision
    /// follows the edits since, as `log` has them; or else, if they can't
    /// be followed, or it held another buffer, it's emptied.
    pub(super) fn bring_up_to(
        &self,
        buffer: BufferId,
        revision: u64,
        tabstop: usize,
        wrap: Option<Wrap>,
        log: &LineLog,
    ) {
        let mut kept = self.0.borrow_mut();
        let key = Some((buffer, tabstop));
        if kept.key != key {
            *kept = KeptLines {
                key,
                revision,
                wrap,
                ..KeptLines::default()
            };
            return;
        }
        if kept.revision != revision {
            match log.since(kept.revision, revision) {
                Some(edits) => edits.into_iter().for_each(|changes| kept.follow(changes)),
                None => kept.clear(),
            }
            kept.revision = revision;
        }
        if kept.wrap != wrap {
            kept.wrap = wrap;
            let mut size = 0;
            for line in kept.lines.values_mut() {
                line.rows = [None, None];
                line.measure();
                size += line.size;
            }
            kept.size = size;
        }
    }

    /// Line `line`, laid out, with tabs stopping at multiples of
    /// `tabstop`: as kept, or else laid out now, from what's kept of it if
    /// it's changed since. Its text is the bytes of `rope` that `bytes`
    /// says, asked only if it's to be laid out.
    pub(super) fn laid(
        &self,
        line: u64,
        rope: &Rope,
        bytes: impl FnOnce() -> (usize, usize),
        tabstop: usize,
    ) -> Rc<Laid> {
        let mut kept = self.0.borrow_mut();
        if let Some(line) = kept.lines.get(&line)
            && line.changed.is_none()
        {
            return Rc::clone(&line.laid);
        }
        let (start, end) = bytes();
        let Some(mut old) = kept.forget(line) else {
            let laid = Rc::new(Laid::new(rope.byte_slice(start..end).to_string(), tabstop));
            kept.keep(line, KeptLine::new(Rc::clone(&laid)));
            return laid;
        };
        let changed = old.changed.take().unwrap_or(Changed {
            from: 0,
            tail: 0,
            len: 0,
        });
        let laid = Rc::unwrap_or_clone(old.laid);
        let from = changed.from.min(end - start).min(laid.text().len());
        let rest =
            (rope.get_byte_slice(start + from..end)).filter(|_| laid.text().is_char_boundary(from));
        let (laid, cell) = match rest {
            Some(rest) => laid.again(rest.chunks(), from, changed.tail, tabstop),
            None => (
                Laid::new(rope.byte_slice(start..end).to_string(), tabstop),
                0,
            ),
        };
        let laid = Rc::new(laid);
        for rows in old.rows.iter_mut().flatten() {
            rows.stale = Some(rows.stale.map_or(cell, |stale| stale.min(cell)));
        }
        old.laid = Rc::clone(&laid);
        old.measure();
        kept.keep(line, old);
        laid
    }

    /// Line `line`'s rows, `laid` as [`laid`](Self::laid) gave it, wrapped
    /// as `wrap` says, with room after its end if `room_after`: as kept, or
    /// else worked out now, from those kept if they're out of date.
    pub(super) fn rows(
        &self,
        line: u64,
        laid: &Laid,
        wrap: Wrap,
        room_after: bool,
    ) -> Rc<[ScreenRow]> {
        let mut kept = self.0.borrow_mut();
        let Some(old) = kept.lines.get_mut(&line) else {
            return wrap::rows(laid, wrap, room_after).into();
        };
        let slot = &mut old.rows[usize::from(room_after)];
        let rows: Rc<[ScreenRow]> = match slot {
            Some(Rows { rows, stale: None }) => return Rc::clone(rows),
            Some(Rows {
                rows,
                stale: Some(stale),
            }) => wrap::rows_again(laid, wrap, room_after, rows, *stale).into(),
            None => wrap::rows(laid, wrap, room_after).into(),
        };
        *slot = Some(Rows {
            rows: Rc::clone(&rows),
            stale: None,
        });
        let was = old.size;
        old.measure();
        let grown = old.size;
        kept.size = kept.size + grown - was;
        kept.trim(line);
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::super::text::Text;
    use super::*;
    use crate::motion::{Lines, Point};

    /// A buffer's text, edited as a copy edits it, noting what each edit
    /// does to its lines, and what one editor's kept of it.
    struct Edited {
        rope: Rope,
        log: LineLog,
        revision: u64,
        kept: Kept,
        tabstop: usize,
        wrap: Option<Wrap>,
    }

    impl Edited {
        fn new(text: &str, tabstop: usize, wrap: Option<Wrap>) -> Self {
            Self {
                rope: Rope::from_str(text),
                log: LineLog::default(),
                revision: 0,
                kept: Kept::default(),
                tabstop,
                wrap,
            }
        }

        fn edit(&mut self, edit: &Edit) {
            let lines = LineChange::of(edit, &self.rope);
            edit.apply(&mut self.rope).unwrap();
            self.revision += 1;
            self.log.push(self.revision, lines);
        }

        /// Replaces `removed`, at byte `at`, with `inserted`.
        fn replace(&mut self, at: usize, removed: &str, inserted: &str) -> Edit {
            let edit = Edit::replace(self.rope.len_bytes(), at, removed, inserted);
            self.edit(&edit);
            edit
        }

        /// The text, laid out into what's kept, typing at `typing`.
        fn text(&self, typing: Option<Point>) -> Text<'_> {
            Text::new(&self.rope, self.tabstop)
                .wrapping(self.wrap, typing)
                .keeping(&self.kept, BufferId(1), self.revision, &self.log)
        }

        /// Checks line `line` is laid out, and wrapped, as it would be
        /// afresh, typing at `typing`.
        fn check(&self, line: u64, typing: Option<Point>, context: &str) {
            let text = self.text(typing);
            let Some(laid) = text.laid(line) else {
                return;
            };
            let fresh = Laid::new(text.line_text(line), self.tabstop);
            laid.assert_as(&fresh, &format!("{context}, line {line}"));
            if let Some(wrap) = self.wrap {
                let room_after = typing.is_some_and(|at| at.line == line && at.col >= laid.cells());
                assert_eq!(
                    *text.screen_rows(line),
                    *wrap::rows_by_cells(&fresh, wrap, room_after),
                    "{context}, line {line}: {:?}",
                    fresh.text(),
                );
            }
        }

        /// Whether anything's kept of line `line`, once what's kept has
        /// followed the edits made.
        fn is_kept(&self, line: u64) -> bool {
            self.kept_laid(line).is_some()
        }

        /// What's kept of line `line`, laid out, as it was last laid out,
        /// once what's kept has followed the edits made.
        fn kept_laid(&self, line: u64) -> Option<Rc<Laid>> {
            drop(self.text(None));
            (self.kept.0.borrow().lines.get(&line)).map(|kept| Rc::clone(&kept.laid))
        }
    }

    /// Wrapping at `width`, between words, keeping lines' indents.
    fn wrap(width: usize) -> Wrap {
        Wrap {
            width,
            linebreak: true,
            breakindent: true,
            showbreak: 0,
        }
    }

    #[test]
    fn lines_laid_out_are_kept_from_frame_to_frame_and_through_edits() {
        let mut edited = Edited::new("a long line\nb\nc\n", 4, Some(wrap(4)));
        let text = edited.text(None);
        let (laid, rows) = (text.laid(0).unwrap(), text.screen_rows(0));
        let (b, c) = (text.laid(1).unwrap(), text.laid(2).unwrap());
        drop(text);
        // Another frame, the same text: the same layout, not done again.
        let text = edited.text(None);
        assert!(Rc::ptr_eq(&laid, &text.laid(0).unwrap()));
        assert!(Rc::ptr_eq(&rows, &text.screen_rows(0)));
        drop((text, laid));
        // A line break typed in the first line: the lines after it move
        // down, as they were.
        edited.replace(6, "", "\n");
        assert_eq!(
            edited.kept_laid(2).map(|b2| Rc::ptr_eq(&b, &b2)),
            Some(true)
        );
        assert_eq!(
            edited.kept_laid(3).map(|c2| Rc::ptr_eq(&c, &c2)),
            Some(true)
        );
        assert!(!edited.is_kept(1), "the new line");
        for line in 0..4 {
            edited.check(line, None, "split");
        }
        // Joined again: the line after goes, and those after move up.
        edited.replace(6, "\n", "");
        assert_eq!(
            edited.kept_laid(1).map(|b1| Rc::ptr_eq(&b, &b1)),
            Some(true)
        );
        assert_eq!(
            edited.kept_laid(2).map(|c2| Rc::ptr_eq(&c, &c2)),
            Some(true)
        );
        for line in 0..3 {
            edited.check(line, None, "joined");
        }
        // Whole lines taken away and put in, as `dd` and `O` do: the line
        // after is as it was.
        edited.replace(0, "a long line\n", "");
        assert_eq!(
            edited.kept_laid(0).map(|b0| Rc::ptr_eq(&b, &b0)),
            Some(true)
        );
        edited.replace(0, "", "a long line\n");
        assert_eq!(
            edited.kept_laid(1).map(|b1| Rc::ptr_eq(&b, &b1)),
            Some(true)
        );
        assert!(!edited.is_kept(0));
        // Other wrapping: the same layout, other rows.
        let laid = edited.text(None).laid(0).unwrap();
        edited.wrap = Some(wrap(5));
        assert!(Rc::ptr_eq(&laid, &edited.text(None).laid(0).unwrap()));
        edited.check(0, None, "rewrapped");
        // A text replaced, which can't be followed: laid out afresh.
        edited.log.clear(edited.revision + 1);
        edited.revision += 1;
        assert!(!Rc::ptr_eq(&laid, &edited.text(None).laid(0).unwrap()));
    }

    #[test]
    fn of_a_line_edited_what_is_before_the_edit_is_kept() {
        let long = "abc 字 ".repeat(2000);
        let mut edited = Edited::new(&format!("{long}\n"), 4, Some(wrap(40)));
        edited.check(0, None, "first");
        // Typed near the end: laid out again from the character before.
        let at = long.len() - 1;
        edited.replace(at, "", "x");
        let old = edited.kept_laid(0).unwrap();
        let (_, cell) =
            (*old)
                .clone()
                .again(edited.rope.byte_slice(at..=long.len()).chunks(), at, 0, 4);
        assert_eq!(cell, old.col_of(at - 1));
        edited.check(0, None, "typed near the end");
        let rows = edited.text(None).screen_rows(0);
        // Rows before the edit's row are kept; a tab typed there moves
        // the rest along.
        edited.replace(at, "", "\t");
        let again = edited.text(None).screen_rows(0);
        assert_eq!(rows[..rows.len() - 2], again[..rows.len() - 2]);
        edited.check(0, None, "a tab typed near the end");
    }

    #[test]
    fn what_is_kept_has_a_budget() {
        // Lines of 70,000 é, each well over a megabyte laid out: those
        // furthest from the last asked for go, within the budget.
        let edited = Edited::new(&format!("{}\n", "é".repeat(70_000)).repeat(20), 4, None);
        let text = edited.text(None);
        for line in 0..20 {
            text.laid(line);
        }
        let kept = edited.kept.0.borrow();
        assert!(kept.size <= KEPT_BYTES, "{} bytes", kept.size);
        assert!(kept.lines.contains_key(&19), "the last asked for");
        assert!(kept.lines.contains_key(&18));
        assert!(!kept.lines.contains_key(&0), "the furthest from it");
        let sizes = kept.lines.values().map(|line| line.size).sum::<usize>();
        assert_eq!(kept.size, sizes);
    }

    /// A xorshift generator, for repeatable edits.
    struct Random(u64);

    impl Random {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            usize::try_from(self.0 % n as u64).unwrap()
        }

        /// Some text of tabs, wide characters, accents, joiners, controls,
        /// blanks, punctuation and line breaks.
        fn text(&mut self) -> String {
            const PIECES: &[&str] = &[
                "a", "bc", " ", "  ", "\t", "字", "é", "\u{301}", "\u{200d}", "\n", "-", ",", ".",
                "\x01", "word", "👩",
            ];
            (0..self.below(5))
                .map(|_| PIECES[self.below(PIECES.len())])
                .collect()
        }

        /// A byte of `rope` where a character starts, or its end.
        fn place(&mut self, rope: &Rope) -> usize {
            rope.char_to_byte(self.below(rope.len_chars() + 1))
        }

        /// What `rope` has from byte `at` for a few characters.
        fn after(&mut self, rope: &Rope, at: usize) -> String {
            let first = rope.byte_to_char(at);
            let last = (first + self.below(4)).min(rope.len_chars());
            rope.slice(first..last).to_string()
        }
    }

    #[test]
    fn what_is_kept_through_any_edits_is_as_laid_out_afresh() {
        for seed in 1..=60_u64 {
            let mut random = Random(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let tabstop = 1 + random.below(8);
            let wrap = (random.below(4) > 0).then(|| Wrap {
                width: 1 + random.below(24),
                linebreak: random.below(2) == 0,
                breakindent: random.below(2) == 0,
                showbreak: random.below(3),
            });
            let start: String = (0..12).map(|_| random.text()).collect();
            let mut edited = Edited::new(&start, tabstop, wrap);
            let mut made: Vec<Edit> = Vec::new();
            for step in 0..120 {
                let context = format!("seed {seed}, step {step}");
                match random.below(10) {
                    // Undone, as the daemon sends an undo.
                    0 if !made.is_empty() => {
                        let edit = made.pop().unwrap().invert();
                        edited.edit(&edit);
                    }
                    // Two changes at once, as `:s` or a macro makes.
                    1 => {
                        let first = random.place(&edited.rope);
                        let removed = random.after(&edited.rope, first);
                        let second = random.place(&edited.rope).max(first + removed.len());
                        let len = edited.rope.len_bytes();
                        if second > len {
                            continue;
                        }
                        let between = edited.rope.byte_slice(first + removed.len()..second);
                        let other = random.after(&edited.rope, second);
                        let mut edit = Edit::new();
                        edit.retain(first)
                            .delete(&removed)
                            .insert(&random.text())
                            .retain(between.len_bytes())
                            .delete(&other)
                            .insert(&random.text())
                            .retain(len - second - other.len());
                        edited.edit(&edit);
                        made.push(edit);
                    }
                    // Whole lines taken away and put in, as `dd` and `p`
                    // of lines do.
                    2 | 3 => {
                        let lines = edited.rope.len_lines();
                        let first = random.below(lines);
                        let last = (first + random.below(3)).min(lines - 1);
                        let at = edited.rope.line_to_byte(first);
                        let removed = (edited.rope)
                            .byte_slice(at..edited.rope.line_to_byte(last))
                            .to_string();
                        let inserted = match random.below(3) {
                            0 => String::new(),
                            1 => format!("{}\n", random.text()),
                            _ => random.text(),
                        };
                        made.push(edited.replace(at, &removed, &inserted));
                    }
                    _ => {
                        let at = random.place(&edited.rope);
                        let removed = random.after(&edited.rope, at);
                        let removed = if random.below(2) == 0 {
                            removed
                        } else {
                            String::new()
                        };
                        made.push(edited.replace(at, &removed, &random.text()));
                    }
                }
                // Looked at now and then, so edits pile up between, and
                // only some lines, so some are laid out and not wrapped.
                if random.below(3) > 0 {
                    let lines = edited.text(None).lines();
                    let typing = (random.below(2) == 0).then(|| {
                        let line =
                            u64::try_from(random.below(usize::try_from(lines).unwrap())).unwrap();
                        Point {
                            line,
                            col: usize::MAX,
                        }
                    });
                    for line in 0..lines {
                        if random.below(4) > 0 {
                            edited.check(line, typing, &context);
                        } else if random.below(2) == 0 {
                            edited.text(None).laid(line);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_line_longer_than_a_row_holds_is_kept_through_edits_past_its_row() {
        for seed in 1..=12_u64 {
            let mut random = Random(seed.wrapping_mul(0x2545_f491_4f6c_dd1d));
            // Plain, or not, or plain but for how it ends: in a character
            // longer, wider or other than a plain one, or an accent.
            let plain = seed % 6 != 1;
            let end =
                ["", "", "é", "字\u{301}", "\x01", "a\u{200d}"][usize::try_from(seed % 6).unwrap()];
            let line = if plain {
                "a=b(c,d);".repeat(8_000) + end
            } else {
                "ab字\tc,".repeat(12_000)
            };
            let mut edited = Edited::new(&format!("{line}\nnext\n"), 4, Some(wrap(37)));
            for step in 0..24 {
                let context = format!("seed {seed}, step {step}");
                let len = edited.text(None).line_end(0);
                // Mostly far along the line, past its row, and sometimes
                // anywhere in it.
                let back = if random.below(4) == 0 {
                    random.below(len + 1)
                } else {
                    random.below(len.min(2_000))
                };
                let at = (edited.rope).char_to_byte(edited.rope.byte_to_char(len - back.min(len)));
                let removed = random.after(&edited.rope, at);
                let removed = if removed.contains('\n') {
                    String::new()
                } else {
                    removed
                };
                let inserted = if plain && random.below(2) == 0 {
                    "x;y".to_owned()
                } else {
                    random.text().replace('\n', "")
                };
                edited.replace(at, &removed, &inserted);
                edited.check(0, None, &context);
                edited.check(1, None, &context);
            }
        }
    }

    #[test]
    fn a_long_line_is_counted_again_only_from_where_it_changed() {
        use super::super::text::COUNTED;
        // How many characters past the row laying out line 0 counts.
        let counted = |edited: &Edited| {
            COUNTED.with(|counted| counted.set(0));
            edited.text(None).laid(0);
            COUNTED.with(std::cell::Cell::get)
        };
        let plain = "a=b(c,d);".repeat(11_000);
        let mut edited = Edited::new(&format!("{plain}\n"), 4, None);
        assert!(counted(&edited) > 30_000, "laid out afresh");
        // A plain line, typed into past its row, or in it, plainly: none.
        edited.replace(80_000, "", "x");
        assert_eq!(counted(&edited), 0);
        edited.replace(30_000, "", "y");
        assert_eq!(counted(&edited), 0);
        assert!(edited.text(None).laid(0).unwrap().is_plain());
        // One that isn't plain, typed into near its end: only its end.
        let other = "ab字\tc,".repeat(14_000);
        let mut edited = Edited::new(&format!("{other}\n"), 4, None);
        counted(&edited);
        let at = other.len() - 9;
        edited.replace(at, "", "z");
        assert!(counted(&edited) < 20);
        edited.check(0, None, "typed near the end");
    }

    #[test]
    fn a_long_line_is_plain_again_once_what_was_not_is_taken_out() {
        let line = "a=b(c,d);".repeat(10_000);
        let mut edited = Edited::new(&format!("{line}\n"), 4, Some(wrap(37)));
        edited.check(0, None, "first");
        assert!(edited.text(None).laid(0).unwrap().is_plain());
        // An é typed past the row: no longer plain, and every character's
        // start is kept.
        edited.replace(80_000, "", "é");
        edited.check(0, None, "é typed");
        let laid = edited.text(None).laid(0).unwrap();
        assert!(!laid.is_plain());
        assert!(laid.keeps_starts());
        drop(laid);
        // Taken out: plain again, keeping nothing of where characters
        // start, so the next key needn't look at what's after it.
        edited.replace(80_000, "é", "");
        edited.check(0, None, "é taken out");
        let laid = edited.text(None).laid(0).unwrap();
        assert!(laid.is_plain());
        assert!(!laid.keeps_starts());
    }

    #[test]
    fn a_log_gives_the_edits_since_a_revision_while_it_has_them() {
        let mut log = LineLog::default();
        let change = |line| {
            Some(
                vec![LineChange {
                    line,
                    byte: 0,
                    removed: 0,
                    inserted: 1,
                    joined: 0,
                    split: 0,
                    whole: false,
                }]
                .into(),
            )
        };
        log.clear(4);
        log.push(5, change(1));
        log.push(6, change(2));
        let lines = |edits: Vec<&[LineChange]>| -> Vec<u64> {
            edits.iter().map(|changes| changes[0].line).collect()
        };
        assert_eq!(log.since(4, 6).map(lines), Some(vec![1, 2]));
        assert_eq!(log.since(5, 6).map(lines), Some(vec![2]));
        assert_eq!(log.since(6, 6).map(lines), Some(vec![]));
        assert_eq!(log.since(3, 6), None, "from before it was cleared");
        assert_eq!(log.since(4, 7), None, "to after its last");
        // One too big to follow can't be followed past.
        log.push(7, None);
        assert_eq!(log.since(4, 7), None);
        assert_eq!(log.since(7, 7).map(lines), Some(vec![]));
        // A gap in the revisions starts it afresh.
        log.push(9, change(3));
        assert_eq!(log.since(7, 9), None);
        assert_eq!(log.since(8, 9).map(lines), Some(vec![3]));
        // It keeps the last few.
        for revision in 10..10 + MOST_EDITS as u64 {
            log.push(revision, change(0));
        }
        assert_eq!(log.edits.len(), MOST_EDITS);
        assert_eq!(log.since(8, 9), None);
    }
}
