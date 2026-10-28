//! `/` `?` `n` `N` `*` `#` in an editor, on the search every pane shares
//! (see [`crate::vim::search`]): Vim's patterns, found in the copy's text,
//! with `incsearch`'s preview and `hlsearch`'s lit matches.
//!
//! A preview, worked out as the window settles before it draws, looks no
//! further than [`REACH`] bytes from the cursor, each way round the file's
//! ends: one that would have to look further, or can't be sure it finds
//! what Enter would (see [`near`]), shows nothing, rather than a match
//! Enter wouldn't go to. A count before `/` isn't shown till it's searched
//! for. Edits from elsewhere while it's typed move where it started, but
//! aren't shown afresh till the next key.

use super::pattern::{Found, Pattern};
use super::wrap::Top;
use super::{
    BufferCopy, Cursor, Editor, Lighting, Matching, Point, Shared, Text, back_matches, find,
    on_matches,
};
use crate::vim::search::{self, Find, Finding, Kinds, Last, Restores, Search, Searcher};

/// How far from the cursor a preview looks for a match, in bytes, before
/// and after going round the file's ends.
pub(super) const REACH: usize = 1 << 20;

/// How far back a preview going backward looks at a time, in bytes.
const CHUNK: usize = 4 << 10;

/// The most matches a preview going backward goes through, as `?.` would
/// go through every character.
const MOST_TRIES: usize = 1 << 14;

/// The kinds an editor's search works in: a match from its first
/// character to its last, and Vim's patterns.
#[derive(Debug)]
pub(crate) struct InFile;

impl Kinds for InFile {
    type Start = Start;
    type Found = (Point, Point);
    type Lit = Lighting;
}

/// Where a search being typed started, to put back.
#[derive(Debug)]
pub(crate) struct Start {
    cursor: Cursor,
    /// The view's top and left, unless the text has been replaced since.
    view: Option<(Top, usize)>,
    /// Where the cursor was in the text, as a byte, before an edit from
    /// elsewhere changes it (see [`Editor::hold`]).
    held: Option<usize>,
}

impl Start {
    /// Where the cursor was.
    pub(super) fn at(&self) -> Point {
        self.cursor.at
    }
}

/// An editor as the shared search drives it, with the copy of the file it
/// shows and what's shared between editors.
pub(super) struct Searching<'a> {
    pub(super) editor: &'a mut Editor,
    pub(super) copy: &'a BufferCopy,
    pub(super) shared: &'a Shared,
}

impl Restores for Editor {
    type Kinds = InFile;

    fn search(&mut self) -> &mut Search<InFile> {
        &mut self.search
    }

    fn keep(&self) -> Start {
        Start {
            cursor: self.cursor,
            view: Some((self.top, self.left)),
            held: None,
        }
    }

    fn put_back(&mut self, start: &mut Start, keep_view: bool) {
        self.cursor = start.cursor;
        match start.view {
            _ if keep_view => {}
            Some(view) => (self.top, self.left) = view,
            // The view then is gone: it goes to the cursor.
            None => self.follow_due = true,
        }
    }
}

impl Restores for Searching<'_> {
    type Kinds = InFile;

    fn search(&mut self) -> &mut Search<InFile> {
        &mut self.editor.search
    }

    fn keep(&self) -> Start {
        Restores::keep(&*self.editor)
    }

    fn put_back(&mut self, start: &mut Start, keep_view: bool) {
        Restores::put_back(&mut *self.editor, start, keep_view);
    }
}

impl Searcher for Searching<'_> {
    /// As a search typed matches case, as `ignorecase` and `smartcase`
    /// say, or a word's as `ignorecase` alone does; with `~` the last
    /// replacement. Compiled once, with the words of the copy's language.
    fn compile(&self, last: &Last) -> Result<Lighting, String> {
        let matching = if last.word {
            Matching::Always(self.editor.case)
        } else {
            Matching::Typed(self.editor.case)
        };
        let pattern = (self.editor).pattern(&last.query, matching, self.copy, self.shared)?;
        let lighting = Lighting::new(&last.query, matching, self.shared);
        *lighting.compiled.borrow_mut() = Some((self.copy.words(), Some(pattern)));
        Ok(lighting)
    }

    fn find(&mut self, find: Find<'_, InFile>) -> Finding<(Point, Point)> {
        let (editor, copy) = (&mut *self.editor, self.copy);
        let Some(pattern) = find.lit.pattern(copy.words()) else {
            return Finding::Missed(String::new());
        };
        let (whole, len) = editor.haystack(copy);
        let hay = &whole[..len];
        let text = editor.text(copy);
        let from = text.byte_at(editor.cursor.at);
        let found = if find.preview.is_some() {
            if find.count > 1 || !copy.loaded() {
                return Finding::Missed(String::new());
            }
            let literal = literal(&find.last.query);
            near(hay, (&pattern, literal), from, find.backward).map(|found| (found, false))
        } else {
            nth(hay, &pattern, from, find.backward, find.count)
        };
        let Some((found, wrapped)) = found else {
            let query = &find.last.query;
            return Finding::Missed(pattern.not_found(format!("{query} isn't in the file")));
        };
        // The last character's first byte.
        let last = (whole[..found.end].char_indices().next_back())
            .map_or(found.start, |(at, _)| at)
            .max(found.start);
        let at = (text.point_of(found.start), text.point_of(last));
        Finding::Found { at, wrapped }
    }

    /// While a search is typed, the view follows at once, as it's put back
    /// from there.
    fn go(&mut self, (start, _): (Point, Point)) {
        self.editor.cursor = Cursor::new(start);
        if self.editor.search.typing() {
            self.editor.follow(self.copy);
        }
    }

    fn say(&mut self, said: String) {
        self.editor.status = Some(said);
    }

    fn fail(&mut self) {
        self.editor.fail();
    }
}

/// The `count`th match of `pattern` in `hay` after byte `from`, or before it
/// if `backward`, going round the ends, as `n` finds it, and whether it went
/// round; `None` if there's none, or the pattern gave up. A count goes a
/// match at a time, as `n` goes, till it's back at the first, and then only
/// as many as are left of a time round: so no count goes round more than
/// once, however big.
fn nth(
    hay: &str,
    pattern: &Pattern,
    from: usize,
    backward: bool,
    count: usize,
) -> Option<(Found, bool)> {
    let found = find(hay, pattern, from, backward)?;
    // A match back where it started went all the way round.
    let wrapped = if backward {
        found.start >= from
    } else {
        found.start <= from
    };
    if count <= 1 {
        return Some((found, wrapped));
    }
    let went = if backward {
        back_matches(hay, pattern, found.start, count - 1)
    } else {
        on_matches(hay, pattern, found, count - 1)
    };
    let (there, round) = went.filter(|_| !pattern.gave_up())?;
    Some((there, wrapped || round))
}

impl Editor {
    /// Shows where the search being typed would go, with `incsearch` (see
    /// [`search::preview`]).
    pub(crate) fn preview_search(&mut self, copy: &BufferCopy, shared: &Shared) {
        let editor = self;
        search::preview(&mut Searching {
            editor,
            copy,
            shared,
        });
    }

    /// Notes where the search being typed started in `text`, before an
    /// edit from elsewhere changes it, as the cursor is.
    pub(super) fn hold_search_start(&mut self, text: &Text<'_>) {
        if let Some(start) = self.search.start_mut() {
            start.held = Some(text.byte_at(start.cursor.at));
        }
    }

    /// Moves where the search being typed started through `edit`, made
    /// since it was held, with the text around it. The match lit then
    /// isn't followed: it's lit no more.
    pub(super) fn search_start_follows(&mut self, edit: &clackatoo_text::Edit) {
        if let Some(start) = self.search.start_mut() {
            start.held = (start.held).map(|at| edit.map(at, clackatoo_text::Bias::Before));
        }
        if let Some(found) = self.search.kept_found_mut() {
            *found = None;
        }
    }

    /// The text is new, as when the file is read again: where the search
    /// being typed started is kept, as far as the text goes now, but not
    /// the view then, nor the match lit, which may be elsewhere now; and
    /// it's shown afresh.
    pub(super) fn search_start_replaced(&mut self, copy: &BufferCopy) {
        let text = self.text(copy);
        if let Some(start) = self.search.start_mut() {
            let line = start.cursor.at.line.min(text.last_line());
            let col = start.cursor.at.col.min(text.line_cells(line));
            start.cursor = Cursor::new(text.point_of(text.byte_at(Point { line, col })));
            (start.view, start.held) = (None, None);
        }
        drop(text);
        if let Some(found) = self.search.kept_found_mut() {
            *found = None;
        }
        self.search.show_again();
    }

    /// Puts where the search being typed started in `text`, edited since
    /// it was held.
    pub(super) fn place_search_start(&mut self, text: &Text<'_>) {
        if let Some(start) = self.search.start_mut()
            && let Some(at) = start.held.take()
        {
            start.cursor = start.cursor.moved(text.point_of(at));
        }
    }
}

/// Where `pattern` next matches in `text` after byte `from`, or before it
/// if `backward`, going round the ends, as [`super::find`] finds it; or
/// `None` if that can't be known within [`REACH`] of `from` each way round.
///
/// It looks in the text cut short at a line's end, so `$` is right there,
/// and a match that can't cross a line break, starting before the cut,
/// can't need what's past it. One that can (a pattern with `\n` in it), or
/// a cut a very long line forced away from its end, could match across the
/// cut where the search Enter makes would, so then only the text to its
/// end is looked through, or nothing is shown.
fn near(
    text: &str,
    (pattern, literal): (&Pattern, bool),
    from: usize,
    backward: bool,
) -> Option<Found> {
    if backward {
        return match last_before(text, (pattern, literal), from) {
            Back::Found(found) => Some(found),
            // Round the end, the last in the text.
            Back::None => match last_before(text, (pattern, literal), text.len() + 1) {
                Back::Found(found) => Some(found),
                Back::None | Back::TooFar => None,
            },
            Back::TooFar => None,
        };
    }
    let after = from
        + text[from.min(text.len())..]
            .chars()
            .next()
            .map_or(1, char::len_utf8);
    if after <= text.len() {
        let end = cut(text, (pattern, literal), (after + REACH).min(text.len()))?;
        if let Some(found) = pattern.find_at(&text[..end], after) {
            return Some(found);
        }
        // What's past where it looked may hold the next.
        if end < text.len() || pattern.gave_up() {
            return None;
        }
    }
    // Round the start, as far as the cursor.
    let end = cut(text, (pattern, literal), REACH.min(from).min(text.len()))?;
    let found = pattern.find_at(&text[..end], 0);
    if found.is_none() && end < from.min(text.len()) {
        return None;
    }
    found
}

/// Where the text is cut, to look through it for `pattern` as far as byte
/// `at`: the end of `at`'s line; or `None` if a match there could cross the
/// cut (see [`near`]). A `literal` one, typed plain ASCII, can be cut
/// anywhere: a match of it crossing the cut ends after any match before
/// the cut that starts after it, so the search to the cut finds the same.
fn cut(text: &str, (pattern, literal): (&Pattern, bool), at: usize) -> Option<usize> {
    let (end, line) = line_end(text, at, REACH);
    (end == text.len() || literal || (line && !pattern.multiline())).then_some(end)
}

/// Whether `source`, a search typed, is plain ASCII text with none of the
/// characters a pattern reads as more, so every match of it is as long.
fn literal(source: &str) -> bool {
    source.is_ascii() && !source.contains(['\\', '.', '*', '[', '~', '^', '$'])
}

/// What looking back for a match found.
enum Back {
    Found(Found),
    /// There's none.
    None,
    /// It can't be known without looking further back than [`REACH`], or
    /// through more than [`MOST_TRIES`] matches.
    TooFar,
}

/// The last match of `pattern` in `text` that starts before byte `before`,
/// as the search Enter makes finds it, one after another from the text's
/// start. Looked for in chunks back from it, each from a line's start, which
/// for a match that can't cross a line break is where the search from the
/// start would be looking too, in the text cut at the end of the line its
/// end is on, so a match going on past it is whole. A pattern that can cross
/// them is looked for from the text's start, in a text no longer than
/// [`REACH`].
fn last_before(text: &str, (pattern, literal): (&Pattern, bool), before: usize) -> Back {
    if pattern.multiline() {
        if text.len() > REACH {
            return Back::TooFar;
        }
        return matches_before(text, pattern, 0, before, &mut 0).unwrap_or(Back::TooFar);
    }
    let (mut high, mut tries) = (before, 0);
    loop {
        let (low, at_line) = line_start(text, high.saturating_sub(CHUNK).min(text.len()), REACH);
        let Some(end) = cut(text, (pattern, literal), high.min(text.len())) else {
            return Back::TooFar;
        };
        if !at_line {
            return Back::TooFar;
        }
        match matches_before(&text[..end], pattern, low, high, &mut tries) {
            Some(Back::None) if low == 0 => return Back::None,
            Some(Back::None) if before.saturating_sub(low) >= REACH => return Back::TooFar,
            Some(Back::None) => high = low,
            Some(back) => return back,
            None => return Back::TooFar,
        }
    }
}

/// The last match of `pattern` in `hay` starting from byte `low` and
/// before `high`, one after another; `None` if it took more than
/// [`MOST_TRIES`] in all, counted in `tries`, or the pattern gave up.
fn matches_before(
    hay: &str,
    pattern: &Pattern,
    low: usize,
    high: usize,
    tries: &mut usize,
) -> Option<Back> {
    let (mut at, mut last) = (low, None);
    while at <= hay.len()
        && let Some(found) = pattern.find_at(hay, at)
        && found.start < high
    {
        *tries += 1;
        if *tries > MOST_TRIES {
            return None;
        }
        last = Some(found);
        at = if found.end > found.start {
            found.end
        } else {
            found.end + hay[found.end..].chars().next().map_or(1, char::len_utf8)
        };
    }
    if pattern.gave_up() {
        return None;
    }
    Some(last.map_or(Back::None, Back::Found))
}

/// The end of the line byte `at` is on, and `true`; or if the line goes on
/// further than `most` bytes past it, a character's start there, and
/// `false`.
fn line_end(text: &str, at: usize, most: usize) -> (usize, bool) {
    let near = (at + most).min(text.len());
    match text.as_bytes()[at..near]
        .iter()
        .position(|&byte| byte == b'\n')
    {
        Some(offset) => (at + offset, true),
        None if near == text.len() => (near, true),
        None => (
            (near..=text.len())
                .find(|&at| text.is_char_boundary(at))
                .unwrap_or(text.len()),
            false,
        ),
    }
}

/// The start of the line byte `at` is on, and `true`; or if the line goes
/// back further than `most` bytes before it, a character's start there, and
/// `false`.
fn line_start(text: &str, at: usize, most: usize) -> (usize, bool) {
    let near = at.saturating_sub(most);
    match text.as_bytes()[near..at]
        .iter()
        .rposition(|&byte| byte == b'\n')
    {
        Some(offset) => (near + offset + 1, true),
        None if near == 0 => (0, true),
        None => (
            (0..=near)
                .rev()
                .find(|&at| text.is_char_boundary(at))
                .unwrap_or(0),
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::Words;

    fn pattern(source: &str) -> Pattern {
        Pattern::new(source, Matching::Exact, "", Words::NONE).unwrap()
    }

    /// Where `source` is found in `text` from the byte `from` marks, as a
    /// preview and as the search Enter makes find it.
    fn both(
        text: &str,
        source: &str,
        from: usize,
        backward: bool,
    ) -> (Option<usize>, Option<usize>) {
        let pattern = pattern(source);
        (
            near(text, (&pattern, literal(source)), from, backward).map(|found| found.start),
            super::super::find(text, &pattern, from, backward).map(|found| found.start),
        )
    }

    #[test]
    fn a_preview_finds_what_the_search_finds_near_the_cursor() {
        let text = "one two\nthree two\nfour";
        for (source, from, backward) in [
            ("two", 0, false),
            ("two", 4, false),
            ("two", 14, false),
            ("one", 5, false),
            ("two", 14, true),
            ("two", 4, true),
            ("four", 0, true),
            ("^t", 0, false),
            ("o$", 0, false),
            ("nothing", 3, false),
            ("nothing", 3, true),
        ] {
            let (preview, search) = both(text, source, from, backward);
            assert_eq!(preview, search, "{source} from {from}, backward {backward}");
        }
    }

    #[test]
    fn a_match_further_than_it_looks_shows_nothing_rather_than_another() {
        let far = "x\n".repeat(REACH);
        // After the cursor, past where it looks: not the one before it,
        // round the start, which the search wouldn't go to.
        let text = format!("needle\n{far}needle\n");
        let (preview, search) = both(&text, "needle", 1, false);
        assert_eq!(search, Some(text.len() - "needle\n".len()));
        assert_eq!(preview, None);
        // Before it, going back, too far.
        let text = format!("needle\n{far}{far}end");
        let (preview, search) = both(&text, "needle", text.len() - 1, true);
        assert_eq!(search, Some(0));
        assert_eq!(preview, None);
        // But near it, found.
        let text = format!("{far}needle here");
        let (preview, search) = both(&text, "needle", text.len() - 2, true);
        assert_eq!((preview, search), (Some(far.len()), Some(far.len())));
    }

    #[test]
    fn where_it_cant_be_sure_it_shows_nothing_rather_than_another() {
        let long = "a".repeat(3 * CHUNK + 1);
        let past_reach = "x".repeat(REACH + 10);
        let lines = "a\nb\n".repeat(REACH / 2);
        for (text, source, from, backward) in [
            // Matches one after another along a line longer than a chunk:
            // from part way along, they'd fall differently.
            (long.as_str(), "aa", long.len() - 1, true),
            // `$` where a very long line was cut short.
            (past_reach.as_str(), "x$", 0, false),
            (past_reach.as_str(), "x$", past_reach.len() - 1, true),
            // A pattern that crosses line breaks, past where it looks.
            (lines.as_str(), "b\\na", 0, false),
            (lines.as_str(), "b\\na", lines.len() - 1, true),
        ] {
            let (preview, search) = both(text, source, from, backward);
            assert!(
                preview.is_none() || preview == search,
                "{source} from {from}: {preview:?}, not {search:?}"
            );
        }
        // Near enough, one that crosses line breaks is found.
        assert_eq!(both("a\nb\na", "b\\na", 0, false), (Some(2), Some(2)));
        assert_eq!(both("a\nb\na\nb", "b\\na", 6, true), (Some(2), Some(2)));
    }

    #[test]
    fn long_lines_are_shown_both_ways() {
        // Lines longer than a chunk: back across them.
        let y = |n| "y".repeat(n);
        let text = format!("{}\n{}foo{}\n{}\n", y(5000), y(2000), y(2997), y(5000));
        let at = 5001 + 2000;
        assert_eq!(
            both(&text, "foo", text.len() - 2, true),
            (Some(at), Some(at))
        );
        // A line longer than it looks along: plain text is found past the
        // cut; a pattern, which `$` or a longer match could fool there, is
        // shown only if it's sure.
        let x = |n| "x".repeat(n);
        let text = format!("{}needle{}", x(REACH / 2), x(2 * REACH));
        let at = REACH / 2;
        assert_eq!(both(&text, "needle", 0, false), (Some(at), Some(at)));
        let (preview, search) = both(&text, "ne.dle", 0, false);
        assert_eq!(search, Some(at));
        assert!(preview.is_none() || preview == search);
    }

    #[test]
    fn going_back_through_every_character_stops_short() {
        let text = "xxxxxxx\n".repeat(MOST_TRIES);
        let pattern = pattern(".");
        // The one just before is found at once, not after all the others.
        assert_eq!(
            near(&text, (&pattern, false), text.len() - 1, true).map(|found| found.start),
            Some(text.len() - 2)
        );
    }
}
