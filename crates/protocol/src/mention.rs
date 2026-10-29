//! Files mentioned in a message to a chat: `@path`, as Claude Code's and
//! Codex's own interfaces have them (CLACKA-193). Every client reads them
//! with [`mentions`], so a window, the phone and the daemon agree on what
//! a message mentions; the daemon sends each one that names a file in the
//! chat's project to its agent as context, and clients show them as links
//! to the file.
//!
//! A mention starts with `@` at the start of the message or after white
//! space, and runs to the next white space: `@src/main.rs`. Punctuation
//! that ends a sentence after it isn't part of it, so `look at @a.rs.`
//! mentions `a.rs`. A path with white space in it is quoted, as Claude
//! Code quotes one: `@"notes/to do.md"`. Either may end with lines,
//! `#L12` or `#L12-L20` (`#L12-20` reads the same), the lines of the file
//! meant, from 1.
//!
//! [`mention`] writes one, quoted only when it must be.

use std::ops::Range;

/// The most mentions a message is read for: those after are words.
pub const MOST_MENTIONS: usize = 32;

/// A file mentioned in a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention<'a> {
    /// Where it is in the message, in bytes, from its `@` to its end,
    /// quotes and lines included.
    pub range: Range<usize>,
    /// The path, as written: relative to the chat's working directory,
    /// unless it's absolute or starts with `~/`.
    pub path: &'a str,
    /// The lines meant, the first and the last, from 1, if it says.
    pub lines: Option<(u32, u32)>,
}

/// What ends a sentence after a mention, and so isn't part of it.
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"', '`'];

/// The files `text`, a message, mentions, in order: at most
/// [`MOST_MENTIONS`].
#[must_use]
pub fn mentions(text: &str) -> Vec<Mention<'_>> {
    let mut found = Vec::new();
    let mut at = 0;
    while found.len() < MOST_MENTIONS {
        let Some(offset) = text[at..].find('@') else {
            break;
        };
        let start = at + offset;
        at = start + 1;
        let after_space = text[..start]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        if !after_space {
            continue;
        }
        if let Some(mention) = read(text, start) {
            at = mention.range.end;
            found.push(mention);
        }
    }
    found
}

/// The mention whose `@` is at `start` in `text`, if one is.
fn read(text: &str, start: usize) -> Option<Mention<'_>> {
    let rest = &text[start + 1..];
    let (written, end) = if let Some(quoted) = rest.strip_prefix('"') {
        let close = quoted.find(['"', '\n'])?;
        if !quoted[close..].starts_with('"') {
            return None;
        }
        let inside = &quoted[..close];
        // Lines may go inside the quotes or after them.
        let after = &quoted[close + 1..];
        let lines_after = after
            .split(char::is_whitespace)
            .next()
            .unwrap_or_default()
            .trim_end_matches(TRAILING);
        let (path, lines) = match mention_lines(lines_after) {
            Some(lines) if lines_after.starts_with('#') => (inside, Some(lines)),
            _ => split_lines(inside),
        };
        let used = if lines.is_some() && lines_after.starts_with('#') {
            lines_after.len()
        } else {
            0
        };
        let end = start + 2 + close + 1 + used;
        ((path, lines), end)
    } else {
        let word = rest.split(char::is_whitespace).next().unwrap_or_default();
        let word = word.trim_end_matches(TRAILING);
        (split_lines(word), start + 1 + word.len())
    };
    let (path, lines) = written;
    let path = path.trim_end_matches('#');
    if path.is_empty() || path.starts_with('@') || path.chars().any(char::is_control) {
        return None;
    }
    Some(Mention {
        range: start..end,
        path,
        lines,
    })
}

/// `written` with lines at its end, `#L12-L20`, taken off, and the lines.
fn split_lines(written: &str) -> (&str, Option<(u32, u32)>) {
    match written.rfind("#L") {
        Some(hash) => match mention_lines(&written[hash..]) {
            Some(lines) => (&written[..hash], Some(lines)),
            None => (written, None),
        },
        None => (written, None),
    }
}

/// The lines `#L12`, `#L12-20` or `#L12-L20` say, first and last, from 1.
#[must_use]
pub fn mention_lines(written: &str) -> Option<(u32, u32)> {
    let lines = written.strip_prefix("#L")?;
    let (first, last) = match lines.split_once('-') {
        Some((first, last)) => (first, last.strip_prefix('L').unwrap_or(last)),
        None => (lines, lines),
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(first) || !digits(last) {
        return None;
    }
    let (first, last): (u32, u32) = (first.parse().ok()?, last.parse().ok()?);
    (first >= 1 && last >= first).then_some((first, last))
}

/// Whether `text`, a message, mentions what looks like a file, with a `/`
/// or a `.` in its path, to label steering with it: steering sends words
/// alone, so the daemon stops the turn and sends a message whole if what
/// it mentions turns out to be a file. Only a guess: the daemon decides by
/// the disk.
#[must_use]
pub fn may_mention_files(text: &str) -> bool {
    mentions(text)
        .iter()
        .any(|mention| mention.path.contains(['/', '.']))
}

/// `lines` as a mention ends with them: `#L12`, or `#L12-L20`.
#[must_use]
pub fn lines_suffix(lines: Option<(u32, u32)>) -> String {
    match lines {
        None => String::new(),
        Some((first, last)) if first == last => format!("#L{first}"),
        Some((first, last)) => format!("#L{first}-L{last}"),
    }
}

/// A mention of `path`, and of its `lines` if given, as [`mentions`] reads
/// it back: quoted if the path has white space or a quote in it, or ends
/// as a sentence does.
#[must_use]
pub fn mention(path: &str, lines: Option<(u32, u32)>) -> String {
    let lines = lines_suffix(lines);
    let quote = path.is_empty()
        || path.contains(char::is_whitespace)
        || path.starts_with('"')
        || path.ends_with(TRAILING);
    if quote {
        format!("@\"{path}{lines}\"")
    } else {
        format!("@{path}{lines}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mention read: its path, its lines, and all of it as written.
    type Read<'a> = (&'a str, Option<(u32, u32)>, &'a str);

    fn read_all(text: &str) -> Vec<Read<'_>> {
        mentions(text)
            .into_iter()
            .map(|found| (found.path, found.lines, &text[found.range]))
            .collect()
    }

    #[test]
    fn a_mention_starts_after_white_space_and_leaves_the_sentence_out() {
        assert_eq!(
            read_all("look at @src/a.rs, then @b.rs."),
            [("src/a.rs", None, "@src/a.rs"), ("b.rs", None, "@b.rs")]
        );
        assert_eq!(read_all("@first word"), [("first", None, "@first")]);
        // Not an address, nor `@` alone, nor two.
        assert!(read_all("mail jack@example.com or @ or @@x").is_empty());
    }

    #[test]
    fn lines_and_quotes() {
        assert_eq!(
            read_all("@a.rs#L3 @b.rs#L4-L9 @c.rs#L5-7 @d.rs#L9-2"),
            [
                ("a.rs", Some((3, 3)), "@a.rs#L3"),
                ("b.rs", Some((4, 9)), "@b.rs#L4-L9"),
                ("c.rs", Some((5, 7)), "@c.rs#L5-7"),
                // Backwards isn't lines.
                ("d.rs#L9-2", None, "@d.rs#L9-2"),
            ]
        );
        assert_eq!(
            read_all("see @\"to do/a b.md#L2-L3\" and @\"x y.rs\"#L4."),
            [
                ("to do/a b.md", Some((2, 3)), "@\"to do/a b.md#L2-L3\""),
                ("x y.rs", Some((4, 4)), "@\"x y.rs\"#L4"),
            ]
        );
        // An unclosed quote isn't one.
        assert!(read_all("@\"open\nquote\"").is_empty());
    }

    #[test]
    fn what_mention_writes_reads_back() {
        for (path, lines) in [
            ("src/main.rs", None),
            ("a b/c.rs", Some((1, 4))),
            ("end.", None),
            ("x.rs", Some((7, 7))),
        ] {
            let written = format!("see {} now", mention(path, lines));
            let read = mentions(&written);
            assert_eq!(read.len(), 1, "{written}");
            assert_eq!((read[0].path, read[0].lines), (path, lines), "{written}");
        }
        assert_eq!(mention("a.rs", Some((2, 5))), "@a.rs#L2-L5");
        assert_eq!(mention("a b.rs", None), "@\"a b.rs\"");
    }

    #[test]
    fn a_path_with_a_dot_or_slash_may_be_a_file() {
        assert!(may_mention_files("look at @src/main"));
        assert!(may_mention_files("look at @main.rs"));
        assert!(!may_mention_files("ask @jack about it"));
        assert!(!may_mention_files("mail jack@example.com"));
    }

    #[test]
    fn mentions_are_read_only_so_far() {
        let many = "@a ".repeat(MOST_MENTIONS + 5);
        assert_eq!(mentions(&many).len(), MOST_MENTIONS);
    }
}
