//! `incsearch`: while `/` or `?` is typed, the cursor goes to the match the
//! search would go to from where it started, the view follows it, and its
//! matches are lit, as Neovim's `incsearch` shows them. Esc puts the
//! cursor, the view and what was lit back; Enter searches from where it
//! started, as without it, so it finds what was shown, and keeps the view.
//!
//! It's worked out as the window settles before it draws, not at each key,
//! so keys typed faster than frames cost one search between them, and a
//! macro's `/foo<CR>`, typed within one, none. Each looks no further than
//! [`REACH`] bytes from the cursor, each way round the file's ends: one
//! that would have to look further shows nothing, rather than a match
//! Enter wouldn't go to.

use super::pattern::{Found, Pattern};
use super::wrap::Top;
use super::{BufferCopy, Cursor, Editor, Lighting, Matching, Point, Prompt, Shared, Text};

/// How far from the cursor a preview looks for a match, in bytes, before
/// and after going round the file's ends.
pub(super) const REACH: usize = 1 << 20;

/// How far back a preview going backward looks at a time, in bytes.
const CHUNK: usize = 4 << 10;

/// The most matches a preview going backward goes through, as `?.` would
/// go through every character.
const MOST_TRIES: usize = 1 << 14;

/// Where a search being typed started, to put back on Esc, and what was
/// last shown of it.
#[derive(Debug)]
pub(super) struct Start {
    cursor: Cursor,
    top: Top,
    left: usize,
    matching: Option<Lighting>,
    found: Option<(Point, Point)>,
    /// What was typed when it was last shown, if it has been.
    previewed: Option<String>,
    /// Where the cursor was in the text, as a byte, before an edit from
    /// elsewhere changes it (see [`Editor::hold`]).
    held: Option<usize>,
}

impl Editor {
    /// A search starts being typed: where it starts is kept, with
    /// `incsearch`, to show it from and put back.
    pub(super) fn start_search_preview(&mut self) {
        self.search_start = self.incsearch.then(|| Start {
            cursor: self.cursor,
            top: self.top,
            left: self.left,
            matching: self.matching.clone(),
            found: self.found,
            previewed: None,
            held: None,
        });
    }

    /// Shows where the search being typed would go, if what's typed has
    /// changed since it was last shown: from where it started, the cursor
    /// going to the match, the view following, and the matches lit. A
    /// pattern that isn't one yet, or no match near enough, shows nothing.
    /// A search no longer being typed, by a way that didn't put things
    /// back, is put back now.
    pub(crate) fn preview_search(&mut self, copy: &BufferCopy, shared: &Shared) {
        let Some(start) = &self.search_start else {
            return;
        };
        let Some(Prompt::Search { line, backward }) = &self.prompt else {
            self.end_search_preview(false);
            return;
        };
        if start.previewed.as_deref() == Some(line.text()) {
            return;
        }
        let (typed, backward) = (line.text().to_owned(), *backward);
        self.put_back_search_start(false);
        if let Some(start) = &mut self.search_start {
            start.previewed = Some(typed.clone());
        }
        if typed.is_empty() || !copy.loaded() {
            return;
        }
        self.found = None;
        let matching = Matching::Typed(self.case);
        let tilde = (shared.substituted.as_ref()).map_or("", |last| last.replacement.as_str());
        let Ok(pattern) = Pattern::new(&typed, matching, tilde, copy.words()) else {
            self.matching = None;
            return;
        };
        self.light_matches(&typed, matching, shared);
        let (whole, len) = self.haystack(copy);
        pattern.searching(&whole);
        let text = self.text(copy);
        let from = text.byte_at(self.cursor.at);
        let Some(found) = near(&whole[..len], &pattern, from, backward) else {
            return;
        };
        // The last character's first byte.
        let last = (whole[..found.end].char_indices().next_back())
            .map_or(found.start, |(at, _)| at)
            .max(found.start);
        self.found = Some((text.point_of(found.start), text.point_of(last)));
        self.cursor = Cursor::new(text.point_of(found.start));
        drop(text);
        self.follow(copy);
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
        if !keep_view {
            (self.top, self.left) = (start.top, start.left);
        }
        self.matching = start.matching.clone();
        self.found = start.found;
    }

    /// Notes where the search being typed started in `text`, before an
    /// edit from elsewhere changes it, as the cursor is.
    pub(super) fn hold_search_start(&mut self, text: &Text<'_>) {
        if let Some(start) = &mut self.search_start {
            start.held = Some(text.byte_at(start.cursor.at));
        }
    }

    /// Moves where the search being typed started through `edit`, made
    /// since it was held, with the text around it. The match lit then
    /// isn't followed: it's lit no more.
    pub(super) fn search_start_follows(&mut self, edit: &clackatoo_text::Edit) {
        if let Some(start) = &mut self.search_start {
            start.held = (start.held).map(|at| edit.map(at, clackatoo_text::Bias::Before));
            start.found = None;
            // Shown afresh in the text as it is now.
            start.previewed = None;
        }
    }

    /// Puts where the search being typed started in `text`, edited since
    /// it was held.
    pub(super) fn place_search_start(&mut self, text: &Text<'_>) {
        if let Some(start) = &mut self.search_start
            && let Some(at) = start.held.take()
        {
            start.cursor = start.cursor.moved(text.point_of(at));
        }
    }
}

/// Where `pattern` next matches in `text` after byte `from`, or before it
/// if `backward`, going round the ends, as [`super::find`] finds it; or
/// `None` if it isn't within [`REACH`] of `from` each way round.
fn near(text: &str, pattern: &Pattern, from: usize, backward: bool) -> Option<Found> {
    if backward {
        return match last_before(text, pattern, from) {
            Back::Found(found) => Some(found),
            // Round the end, the last in the text.
            Back::None => match last_before(text, pattern, text.len() + 1) {
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
        let end = line_end(text, (after + REACH).min(text.len()), REACH);
        if let Some(found) = pattern.find_at(&text[..end], after) {
            return Some(found);
        }
        // What's past where it looked may hold the next.
        if end < text.len() || pattern.gave_up() {
            return None;
        }
    }
    // Round the start, as far as the cursor.
    let end = line_end(text, REACH.min(from).min(text.len()), REACH);
    let found = pattern.find_at(&text[..end], 0);
    if found.is_none() && end < from.min(text.len()) {
        return None;
    }
    found
}

/// What looking back for a match found.
enum Back {
    Found(Found),
    /// There's none.
    None,
    /// It would have to look further back than [`REACH`], or through more
    /// than [`MOST_TRIES`] matches, to know.
    TooFar,
}

/// The last match of `pattern` in `text` that starts before byte `before`,
/// looked for in chunks back from it, from a line's start, each in the
/// text to the end of the line its end is on, so a match going on past it
/// is whole.
fn last_before(text: &str, pattern: &Pattern, before: usize) -> Back {
    let (mut high, mut tries) = (before, 0);
    loop {
        let low = line_start(text, high.saturating_sub(CHUNK).min(text.len()), CHUNK);
        let hay = &text[..line_end(text, high.min(text.len()), REACH)];
        let (mut at, mut last) = (low, None);
        while at <= hay.len()
            && let Some(found) = pattern.find_at(hay, at)
            && found.start < high
        {
            tries += 1;
            if tries > MOST_TRIES {
                return Back::TooFar;
            }
            last = Some(found);
            at = if found.end > found.start {
                found.end
            } else {
                found.end + hay[found.end..].chars().next().map_or(1, char::len_utf8)
            };
        }
        if pattern.gave_up() {
            return Back::TooFar;
        }
        match last {
            Some(found) => return Back::Found(found),
            None if low == 0 => return Back::None,
            None if before.saturating_sub(low) >= REACH => return Back::TooFar,
            None => high = low,
        }
    }
}

/// The end of the line byte `at` is on, or `most` bytes past it at a
/// character's start, if the line goes on further.
fn line_end(text: &str, at: usize, most: usize) -> usize {
    let near = (at + most).min(text.len());
    match text.as_bytes()[at..near]
        .iter()
        .position(|&byte| byte == b'\n')
    {
        Some(offset) => at + offset,
        None => (near..=text.len())
            .find(|&at| text.is_char_boundary(at))
            .unwrap_or(text.len()),
    }
}

/// The start of the line byte `at` is on, or `most` bytes before it at a
/// character's start, if the line goes back further.
fn line_start(text: &str, at: usize, most: usize) -> usize {
    let near = at.saturating_sub(most);
    match text.as_bytes()[near..at]
        .iter()
        .rposition(|&byte| byte == b'\n')
    {
        Some(offset) => near + offset + 1,
        None => (0..=near)
            .rev()
            .find(|&at| text.is_char_boundary(at))
            .unwrap_or(0),
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
            near(text, &pattern, from, backward).map(|found| found.start),
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
    fn going_back_through_every_character_stops_short() {
        let text = "x".repeat(4 * MOST_TRIES);
        let pattern = pattern(".");
        // The one just before is found at once, not after all the others.
        assert_eq!(
            near(&text, &pattern, text.len() - 1, true).map(|found| found.start),
            Some(text.len() - 2)
        );
    }
}
