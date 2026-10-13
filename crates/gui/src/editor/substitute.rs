//! `:s`: what it's typed as, what it puts in, and where, as Vim's does.
//!
//! The matches are found in the text as it is, and the whole command makes
//! one [`Edit`], so `u` undoes it in one step, and the daemon is sent one
//! change however many lines it touches.
//!
//! Each line is searched from its start. Without `g`, a line's first match
//! is replaced; with it, each, as in Vim: an empty match where the last
//! ended is skipped, and none is tried at a line's end once a match has
//! been found on it. A pattern that takes line breaks can match past its
//! line's end, and the search goes on from where that match ended. The
//! break that ends the file's last line is never matched, so `:%s/\n//`
//! joins the lines but leaves the file ending with one.

use clackatoo_text::Edit;
use regex::CaptureLocations;

use super::case::Recase;
use super::pattern::{Found, Pattern};

/// What `:s` is typed with after its name: `/pattern/replacement/flags`
/// and a count, any of it left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Typed {
    /// The pattern, if one was typed; empty for the last search's.
    pub(crate) pattern: Option<String>,
    /// The replacement, if a pattern was typed: empty if it wasn't.
    pub(crate) replacement: String,
    /// Whether the replacement was begun, with the delimiter after the
    /// pattern, even if it's empty.
    pub(crate) replacing: bool,
    /// The flags, as typed, read once the last `:s`'s are known.
    pub(crate) flags: String,
    /// How many lines, from the range's last, if a count was typed.
    pub(crate) count: Option<u64>,
}

/// `:s`'s flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
// Flags, as Vim's are, not states of one machine.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct Flags {
    /// `g`: every match on a line, not just the first.
    pub(crate) all: bool,
    /// `c`: ask about each match.
    pub(crate) confirm: bool,
    /// `n`: count the matches, replacing none.
    pub(crate) count_only: bool,
    /// `e`: say nothing if there's no match.
    pub(crate) quiet: bool,
    /// `i` (`Some(false)`) or `I` (`Some(true)`): match case exactly, or
    /// not, whatever the options say.
    pub(crate) exact: Option<bool>,
    /// `r`: with no pattern, the last search's, not the last `:s`'s.
    pub(crate) last_search: bool,
}

impl Flags {
    /// The flags written in `text`, as after `:s/a/b/`, starting from
    /// `last`, the last `:s`'s, if they start with `&`. As in Vim, `g`,
    /// `c` and `e` turn what they say over, so `:&&g` after `:s/a/b/g`
    /// replaces only the first match.
    pub(crate) fn read(text: &str, last: Self) -> Result<Self, String> {
        let (mut flags, text) = match text.strip_prefix('&') {
            Some(text) => (last, text),
            None => (Self::default(), text),
        };
        flags.last_search = false;
        for ch in text.chars() {
            match ch {
                'g' => flags.all = !flags.all,
                'c' => flags.confirm = !flags.confirm,
                'e' => flags.quiet = !flags.quiet,
                'n' => flags.count_only = true,
                'i' => flags.exact = Some(false),
                'I' => flags.exact = Some(true),
                'r' => flags.last_search = true,
                // Printing the line, which the bar does anyway.
                'p' | '#' | 'l' => {}
                _ => {
                    return Err(format!(
                        "{ch} isn't one of :s's flags, which are &, c, e, g, i, I, n and r"
                    ));
                }
            }
        }
        Ok(flags)
    }
}

/// Reads what follows `:s`'s name.
pub(crate) fn read(text: &str) -> Result<Typed, String> {
    let text = text.trim_start();
    let Some(delimiter) = text.chars().next() else {
        return Ok(Typed::default());
    };
    if delimiter.is_ascii_alphanumeric() || delimiter == '&' {
        // `:s g`, `:s 3`, `:s&`: the last again, with these.
        return tail(text, Typed::default());
    }
    if matches!(delimiter, '\\' | '"' | '|') {
        return Err(format!("{delimiter} can't separate :s's parts"));
    }
    let rest = &text[delimiter.len_utf8()..];
    let (pattern, rest) = split(rest, delimiter);
    let replacing = rest.is_some();
    let (replacement, rest) = match rest {
        Some(rest) => {
            let (replacement, rest) = split(rest, delimiter);
            (replacement, rest.unwrap_or_default())
        }
        None => (String::new(), ""),
    };
    let typed = Typed {
        pattern: Some(pattern),
        replacement,
        replacing,
        ..Typed::default()
    };
    tail(rest, typed)
}

/// Reads the flags and count at the end of `:s`, into `typed`.
fn tail(text: &str, mut typed: Typed) -> Result<Typed, String> {
    let text = text.trim_start();
    let flags_end = text
        .find(|ch: char| ch.is_whitespace() || ch.is_ascii_digit())
        .unwrap_or(text.len());
    let flags = &text[..flags_end];
    // Checked now, so a mistake is told of before anything's done.
    Flags::read(flags, Flags::default())?;
    flags.clone_into(&mut typed.flags);
    let count = text[flags_end..].trim();
    if !count.is_empty() {
        match count.parse::<u64>() {
            Ok(count) if count > 0 => typed.count = Some(count),
            _ => return Err(format!("{count} isn't a count of lines")),
        }
    }
    Ok(typed)
}

/// `text` up to the first `delimiter` without a backslash, and what's
/// after that, if there's one. A backslash before the delimiter is kept,
/// as both patterns and replacements read `\/` and its kin as the
/// character itself.
fn split(text: &str, delimiter: char) -> (String, Option<&str>) {
    let mut part = String::with_capacity(text.len());
    let mut chars = text.char_indices();
    while let Some((at, ch)) = chars.next() {
        if ch == delimiter {
            return (part, Some(&text[at + ch.len_utf8()..]));
        }
        part.push(ch);
        if ch == '\\'
            && let Some((_, next)) = chars.next()
        {
            part.push(next);
        }
    }
    (part, None)
}

/// `source`, a replacement, with each `~` without a backslash standing for
/// `previous`, the last replacement, as Vim reads it before anything else.
pub(crate) fn expand_tilde(source: &str, previous: &str) -> String {
    let mut expanded = String::with_capacity(source.len());
    let mut chars = source.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                expanded.push('\\');
                if let Some(next) = chars.next() {
                    expanded.push(next);
                }
            }
            '~' => expanded.push_str(previous),
            _ => expanded.push(ch),
        }
    }
    expanded
}

/// A piece of a replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Text(String),
    /// What one of the pattern's groups matched: 0 for the whole match,
    /// `&` or `\0`, and 1 to 9 for `\1` to `\9`.
    Group(usize),
    /// A line break, `\r`, and `\n` too (see [`Replacement`]).
    Break,
    /// `\u` or `\l`: the next character's case changed.
    One(Recase),
    /// `\U` or `\L`, until `\E` or `\e`, `None`.
    All(Option<Recase>),
}

/// What `:s` puts in place of each match.
///
/// `&` and `\0` are the match, `\1` to `\9` its groups, `\r` a line break,
/// `\t` a tab, and `\u`, `\l`, `\U`, `\L`, `\E` and `\e` change case, as
/// in Vim. So does `\n`, which in Vim puts in a NUL character, written to
/// the file as a zero byte, which is never what's meant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Replacement {
    pieces: Vec<Piece>,
    /// Whether it changes case, which is done a character at a time.
    cased: bool,
}

impl Replacement {
    /// Reads `source`, its `~`s already expanded.
    pub(crate) fn new(source: &str) -> Result<Self, String> {
        if source.starts_with("\\=") {
            return Err("\\= expressions in a replacement aren't supported".into());
        }
        let mut pieces = Vec::new();
        let mut text = String::new();
        let push = |pieces: &mut Vec<Piece>, text: &mut String, piece| {
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(text)));
            }
            pieces.push(piece);
        };
        let mut chars = source.chars();
        while let Some(ch) = chars.next() {
            let piece = match ch {
                '&' => Piece::Group(0),
                '\r' | '\n' => Piece::Break,
                '\\' => match chars.next() {
                    Some(digit @ '0'..='9') => Piece::Group(digit as usize - '0' as usize),
                    Some('n' | 'r') => Piece::Break,
                    Some('t') => {
                        text.push('\t');
                        continue;
                    }
                    Some('u') => Piece::One(Recase::Upper),
                    Some('l') => Piece::One(Recase::Lower),
                    Some('U') => Piece::All(Some(Recase::Upper)),
                    Some('L') => Piece::All(Some(Recase::Lower)),
                    Some('E' | 'e') => Piece::All(None),
                    // Any other is itself: `\&`, `\~`, `\\`, `\/`.
                    Some(other) => {
                        text.push(other);
                        continue;
                    }
                    None => {
                        text.push('\\');
                        continue;
                    }
                },
                _ => {
                    text.push(ch);
                    continue;
                }
            };
            push(&mut pieces, &mut text, piece);
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text));
        }
        let cased = (pieces.iter()).any(|piece| matches!(piece, Piece::One(_) | Piece::All(_)));
        Ok(Self { pieces, cased })
    }

    /// Whether it puts in what one of the pattern's groups matched, `\1`
    /// to `\9`, which is slower to find than the match alone.
    pub(crate) fn uses_groups(&self) -> bool {
        (self.pieces.iter()).any(|piece| matches!(piece, Piece::Group(1..)))
    }

    /// Adds what replaces `found`, the match `pattern` found in `text`,
    /// whose groups are in `locations` if it [uses](Self::uses_groups)
    /// them, to `out`.
    pub(crate) fn expand(
        &self,
        text: &str,
        pattern: &Pattern,
        (found, locations): (Found, &CaptureLocations),
        out: &mut String,
    ) {
        let group = |number: usize| {
            let group = match number {
                0 => Some(found),
                _ => pattern.group(locations, number),
            };
            group.map_or("", |Found { start, end }| &text[start..end])
        };
        if !self.cased {
            for piece in &self.pieces {
                match piece {
                    Piece::Text(text) => out.push_str(text),
                    Piece::Group(number) => out.push_str(group(*number)),
                    Piece::Break => out.push('\n'),
                    Piece::One(_) | Piece::All(_) => {}
                }
            }
            return;
        }
        let (mut one, mut all) = (None::<Recase>, None::<Recase>);
        let mut push = |text: &str, one: &mut Option<Recase>, all: Option<Recase>| {
            for ch in text.chars() {
                let ch = match (one.take(), all) {
                    (Some(recase), _) | (None, Some(recase)) => recase.char(ch),
                    (None, None) => ch,
                };
                out.push(ch);
            }
        };
        for piece in &self.pieces {
            match piece {
                Piece::Text(text) => push(text, &mut one, all),
                Piece::Group(number) => push(group(*number), &mut one, all),
                Piece::Break => push("\n", &mut None, None),
                Piece::One(recase) => one = Some(*recase),
                Piece::All(recase) => all = *recase,
            }
        }
    }
}

/// Lines to search: from the byte the first starts at to the byte the last
/// ends at, before its line break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bytes {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// Has `visit` see each match `:s` replaces in `lines` of `text`, every one
/// on a line if `all`, in order, with where its groups are if `groups`
/// asks for them, until it says to stop. `text` leaves out the line break
/// that ends the file's last line, if it ends with one, which is never
/// matched.
pub(crate) fn each_match(
    text: &str,
    pattern: &Pattern,
    lines: Bytes,
    (all, groups): (bool, bool),
    visit: &mut dyn FnMut(Found, &CaptureLocations) -> bool,
) {
    let mut locations = pattern.locations();
    let line_end = |from: usize| text[from..].find('\n').map_or(text.len(), |at| from + at);
    let mut at = lines.start;
    // Where the line being searched ends, and where the last match on it
    // ended.
    let mut end = line_end(at);
    let mut last: Option<usize> = None;
    while at <= lines.end {
        let found = if groups {
            pattern.captures_at(text, at, &mut locations)
        } else {
            pattern.find_at(text, at)
        };
        let Some(found) = found else {
            break;
        };
        if found.start > lines.end {
            break;
        }
        if found.start > end {
            // No more on this line: on from the line it's on.
            end = line_end(found.start);
            last = None;
        }
        let empty = found.start == found.end;
        let next_line = |end: usize| (end + 1, line_end((end + 1).min(text.len())));
        if empty && last == Some(found.start) {
            // Vim did this one: on from the next character.
            at = next_char(text, found.start);
            if at > end || (at == end && !pattern.multiline()) {
                (at, end) = next_line(end);
                last = None;
            }
            continue;
        }
        if !visit(found, &locations) {
            return;
        }
        let ends_line = found.end > end;
        if !all {
            if ends_line && text.as_bytes()[found.end - 1] == b'\n' {
                // It took the line's break: the next line is searched, as
                // the rest of this one.
                at = found.end;
                end = line_end(at);
            } else {
                if ends_line {
                    end = line_end(found.end);
                }
                (at, end) = next_line(end);
            }
            last = None;
            continue;
        }
        if ends_line {
            end = line_end(found.end);
        }
        // From the match's end, even if it's empty: an empty match there
        // is then skipped, as Vim did it, which `\zs` needs.
        last = Some(found.end);
        at = found.end;
        // None is tried at the line's end once one's been found on it,
        // unless the pattern takes the line's break.
        if at > end || (at == end && !pattern.multiline()) {
            (at, end) = next_line(end);
            last = None;
        }
    }
}

/// The byte after the character at `at` in `text`, or one past its end.
fn next_char(text: &str, at: usize) -> usize {
    at + text[at.min(text.len())..]
        .chars()
        .next()
        .map_or(1, char::len_utf8)
}

/// What `:s` did, or would do.
#[derive(Debug, Default)]
pub(crate) struct Replaced {
    /// The edit that makes it, of the whole text.
    pub(crate) edit: Edit,
    /// How many matches were replaced, and on how many lines.
    pub(crate) count: usize,
    pub(crate) lines: usize,
    /// Where the last replacement ends, in the text as edited.
    pub(crate) last_end: Option<usize>,
}

/// `:s` in `lines` of `text`, which starts at byte `base` of the whole
/// text, `len` bytes long. `text` is all of it from there, or just the
/// lines, if the pattern can't cross them, and never takes in the line
/// break that ends the last line.
pub(crate) fn replace(
    text: &str,
    (base, len): (usize, usize),
    pattern: &Pattern,
    replacement: &Replacement,
    lines: Bytes,
    all: bool,
) -> Replaced {
    let mut replaced = Replaced::default();
    replaced.edit.retain(base);
    // How far through the text the edit has got, and how much longer the
    // text is for it so far.
    let mut through = 0;
    let mut longer = 0isize;
    let mut last_start = None;
    let mut with = String::new();
    let groups = replacement.uses_groups();
    each_match(
        text,
        pattern,
        lines,
        (all, groups),
        &mut |found, locations| {
            with.clear();
            replacement.expand(text, pattern, (found, locations), &mut with);
            (replaced.edit)
                .retain(found.start - through)
                .delete(&text[found.start..found.end])
                .insert(&with);
            through = found.end;
            let start = found.start.cast_signed() + longer;
            longer += with.len().cast_signed() - (found.end - found.start).cast_signed();
            replaced.last_end = Some(base + start.cast_unsigned() + with.len());
            let new_line =
                last_start.is_none_or(|last: usize| text[last..found.start].contains('\n'));
            replaced.lines += usize::from(new_line);
            last_start = Some(found.start);
            replaced.count += 1;
            true
        },
    );
    replaced.edit.retain(len - base - through);
    replaced
}

/// What the bar says of `count` matches on `lines` lines, as Vim does:
/// substitutions made, or if `matched`, matches counted.
pub(crate) fn report(count: usize, lines: usize, matched: bool) -> String {
    let (one, many) = if matched {
        ("match", "matches")
    } else {
        ("substitution", "substitutions")
    };
    let things = if count == 1 { one } else { many };
    let line = if lines == 1 { "line" } else { "lines" };
    format!("{count} {things} on {lines} {line}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::pattern::Matching;

    /// `:s/pattern/replacement/`, with `g` if `all`, on every line of
    /// `text`, as Vim does it.
    fn substitute(text: &str, pattern: &str, replacement: &str, all: bool) -> String {
        let pattern = Pattern::new(pattern, Matching::Exact, "").unwrap();
        let replacement = Replacement::new(replacement).unwrap();
        let hay = text.strip_suffix('\n').unwrap_or(text);
        let lines = Bytes {
            start: 0,
            end: hay.len(),
        };
        let replaced = replace(hay, (0, text.len()), &pattern, &replacement, lines, all);
        replaced.edit.apply_to(text).unwrap()
    }

    #[test]
    fn empty_matches_go_as_vims_do() {
        // As Neovim does each.
        assert_eq!(substitute("xxa\nabc\n", "x*", "-", true), "-a\n-a-b-c\n");
        assert_eq!(substitute("abc\n", "b*", "-", true), "-a-c\n");
        assert_eq!(substitute("abc\n", "$", "!", true), "abc!\n");
        assert_eq!(substitute("ab\ncd\n", "$", ";", false), "ab;\ncd;\n");
        assert_eq!(substitute("a\n\nb\n", "^$", "E", false), "a\nE\nb\n");
        assert_eq!(substitute("a a a\n", "a", "b", false), "b a a\n");
        // An empty match after `\zs` steps no further than Vim's.
        assert_eq!(substitute("foofoo\n", "foo\\zs", "X", true), "fooXfooX\n");
    }

    #[test]
    fn line_breaks_are_matched_but_not_the_files_last() {
        assert_eq!(substitute("a\nb\nc\n", "\\n", "", false), "abc\n");
        assert_eq!(substitute("a,b\nc\n", ",", "\\r", false), "a\nb\nc\n");
        assert_eq!(substitute("a,b\n", ",", "\\n", false), "a\nb\n");
        assert_eq!(substitute("a\nb\nc", "\\n", "-", true), "a-b-c");
    }

    #[test]
    fn replacements_have_vims_specials() {
        assert_eq!(
            substitute(
                "foo bar\n",
                "\\(foo\\) \\(bar\\)",
                "\\u\\1-\\U\\2\\E!-&-\\0",
                false
            ),
            "Foo-BAR!-foo bar-foo bar\n"
        );
        assert_eq!(
            substitute("HELLO world", "\\w\\+", "\\L\\u&", true),
            "Hello World"
        );
        assert_eq!(substitute("abc def", "\\w\\+", "\\U\\l&", true), "aBC dEF");
        assert_eq!(substitute("a&b", "&", "\\&\\&", false), "a&&b");
        assert_eq!(substitute("a,b", ",", "\\t", false), "a\tb");
        assert_eq!(substitute("a/b", "/", "\\/|", false), "a/|b");
        assert_eq!(substitute("aXa", "x\\c", "[&]", false), "a[X]a");
        assert_eq!(
            substitute("foobar foobaz", "foo\\zsba", "XX", true),
            "fooXXr fooXXz"
        );
        assert_eq!(expand_tilde("~Y\\~", "X"), "XY\\~");
        assert!(Replacement::new("\\=1+1").is_err());
    }

    #[test]
    fn what_follows_the_name_is_read_as_vim_reads_it() {
        let typed = read("/a\\/b/c\\/d/gI 3").unwrap();
        assert_eq!(typed.pattern.as_deref(), Some("a\\/b"));
        assert_eq!(typed.replacement, "c\\/d");
        assert_eq!(typed.flags, "gI");
        assert_eq!(typed.count, Some(3));
        let typed = read("#a#b").unwrap();
        assert_eq!(
            (typed.pattern.as_deref(), typed.replacement.as_str()),
            (Some("a"), "b")
        );
        let typed = read("/b").unwrap();
        assert_eq!(
            (typed.pattern.as_deref(), typed.replacement.as_str()),
            (Some("b"), "")
        );
        let typed = read("").unwrap();
        assert_eq!(typed.pattern, None);
        let typed = read(" &g 2").unwrap();
        assert!(typed.pattern.is_none() && typed.flags == "&g");
        assert_eq!(typed.count, Some(2));
        assert!(read("/a/b/z").is_err());
        assert!(read("/a/b/g 0").is_err());
        // `&` keeps the last flags, and `g` again turns `g` off.
        let last = Flags::read("gc", Flags::default()).unwrap();
        let now = Flags::read("&g", last).unwrap();
        assert!(!now.all && now.confirm);
        assert!(!Flags::read("g", last).unwrap().confirm);
        assert_eq!(report(1, 1, false), "1 substitution on 1 line");
        assert_eq!(report(3, 2, true), "3 matches on 2 lines");
    }
}
