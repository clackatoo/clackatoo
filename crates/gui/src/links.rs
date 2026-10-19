//! Links in a terminal's text: what they point to, and finding the ones a
//! program printed as plain text.
//!
//! Programs that know they're printing a link say so with OSC 8 (see
//! [`clackatoo_term::Hyperlink`]), and the daemon sends those with the
//! cells. Most don't, so the window also looks for links in the text
//! itself, on the fly and only where it's asked to: under the mouse while
//! Ctrl is held, or at the cursor for `gx`. Two kinds are found:
//!
//! - URLs: `http://`, `https://`, `file://` and `mailto:`.
//! - Paths, optionally followed by `:line` and `:line:col`, as compilers
//!   (`--> src/main.rs:12:5`), grep (`src/main.rs:12:`) and stack traces
//!   print them. To keep words such as `e.g.` from being taken for files,
//!   a path must have a `/` in it, or a line number after a name with an
//!   extension.
//!
//! Trailing punctuation that ends a sentence, and brackets that close
//! around a link rather than belong to it, are left out.

use std::ops::Range;

/// What a link points to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// A URL, for the desktop to open.
    Url(String),
    /// A file, on the daemon's machine, for an editor pane: `path` is
    /// relative to the terminal's working directory unless it's absolute,
    /// and `line` and `col` count from 1, as compilers print them.
    File {
        path: String,
        line: Option<u64>,
        col: Option<u64>,
    },
    /// A file on another machine than the daemon's, which can't be opened:
    /// a `file://` URI naming `host`.
    Elsewhere { uri: String, host: String },
}

/// A link found in some text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    /// Where it is in the text, in bytes.
    pub(crate) bytes: Range<usize>,
    pub(crate) target: Target,
}

/// The URI schemes a link may have to be opened by the desktop: those of
/// the web, mail, and fetching files, as kitty's default list has them.
/// Others are left alone, so text a program prints can't have the
/// desktop's opener start whatever handles an odd scheme.
const OPENABLE: &[&str] = &[
    "http", "https", "mailto", "ftp", "ftps", "sftp", "ssh", "git", "gemini", "gopher", "irc",
    "ircs", "news",
];

/// The schemes plain text is searched for, with what follows them.
const FOUND_SCHEMES: &[&str] = &["https://", "http://", "file://", "mailto:"];

/// What a URI points to, if it's a kind that can be opened: a `file://`
/// URI is a file on the daemon's machine if it names no host, or
/// `localhost` (the daemon leaves out its own name, see
/// `clackatoo_term::Config::local_host`), and elsewhere if it names
/// another; a URL with a scheme in [`OPENABLE`] is a URL.
pub(crate) fn target_of_uri(uri: &str) -> Option<Target> {
    if let Some(rest) = uri.strip_prefix("file://") {
        // The host, if any, is up to the path's first slash.
        let (host, path) = rest.split_at(rest.find('/')?);
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            return Some(Target::Elsewhere {
                uri: uri.to_owned(),
                host: host.to_owned(),
            });
        }
        // A fragment of digits is a line, as kitty's hyperlinked grep
        // writes it.
        let (path, fragment) = path.split_once('#').unwrap_or((path, ""));
        let line = (fragment.trim_start_matches('L').parse().ok()).filter(|&line| line > 0);
        return Some(Target::File {
            path: percent_decode(path),
            line,
            col: None,
        });
    }
    let (scheme, rest) = uri.split_once(':')?;
    let openable = OPENABLE
        .iter()
        .any(|known| known.eq_ignore_ascii_case(scheme));
    (openable && !rest.is_empty()).then(|| Target::Url(uri.to_owned()))
}

/// The link in `text` that covers byte `at`, if any.
pub(crate) fn find_at(text: &str, at: usize) -> Option<Found> {
    url_at(text, at).or_else(|| path_at(text, at))
}

/// Every link in `text`, in order.
#[cfg(test)]
pub(crate) fn find_all(text: &str) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    let mut at = 0;
    while at < text.len() {
        match find_at(text, at) {
            Some(link) if link.bytes.end > at => {
                at = link.bytes.end;
                found.push(link);
            }
            _ => at += text[at..].chars().next().map_or(1, char::len_utf8),
        }
    }
    found
}

/// The URL covering byte `at`.
fn url_at(text: &str, at: usize) -> Option<Found> {
    // URLs don't hold spaces, so the one covering `at` starts after the
    // last space before it.
    let word = text[..at]
        .rfind(char::is_whitespace)
        .map_or(0, |space| space + 1);
    let mut search = word;
    while let Some((start, scheme)) = next_scheme(text, search) {
        if start > at {
            return None;
        }
        search = start + scheme.len();
        // A scheme in the middle of a word ("xhttp://") isn't one.
        let after_letter = text[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        if after_letter {
            continue;
        }
        let end = start + url_len(&text[start..]);
        if end <= start + scheme.len() {
            continue;
        }
        if (start..end).contains(&at) {
            // A `file://` URL with nothing after the host goes nowhere.
            let target = target_of_uri(&text[start..end])?;
            return Some(Found {
                bytes: start..end,
                target,
            });
        }
    }
    None
}

/// Where the next scheme of [`FOUND_SCHEMES`] starts at or after `from`,
/// and which it is.
fn next_scheme(text: &str, from: usize) -> Option<(usize, &'static str)> {
    let rest = &text[from..];
    FOUND_SCHEMES
        .iter()
        .filter_map(|scheme| {
            let found = find_ascii_case_insensitive(rest, scheme)?;
            Some((from + found, *scheme))
        })
        .min_by_key(|&(start, _)| start)
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// How much of `text`, which starts with a URL, is the URL: up to a space
/// or a character URLs never hold, less the punctuation and closing
/// brackets that end the text around it.
fn url_len(text: &str) -> usize {
    let end = text
        .find(|ch: char| ch.is_whitespace() || ch.is_control() || "<>\"'`{}|\\^".contains(ch))
        .unwrap_or(text.len());
    trim_end(&text[..end]).len()
}

/// `text` less the trailing characters that more likely end the sentence
/// or bracket around it than belong to it: `.`, `,`, `:`, `;`, `!`, `?`,
/// and closing brackets with nothing to close.
fn trim_end(text: &str) -> &str {
    let mut text = text;
    loop {
        let Some(last) = text.chars().next_back() else {
            return text;
        };
        let trimmed = match last {
            '.' | ',' | ':' | ';' | '!' | '?' | '*' => true,
            ')' | ']' => {
                let open = if last == ')' { '(' } else { '[' };
                let opens = text.matches(open).count();
                let closes = text.matches(last).count();
                closes > opens
            }
            _ => false,
        };
        if !trimmed {
            return text;
        }
        text = &text[..text.len() - last.len_utf8()];
    }
}

/// Whether `ch` can be part of a path as it's found in text. Spaces,
/// quotes, brackets and commas end one, as they do around paths in
/// compilers' and tools' messages.
fn is_path_char(ch: char) -> bool {
    ch.is_alphanumeric() || "_-./~+@%".contains(ch)
}

/// The path covering byte `at`, with its line and column.
fn path_at(text: &str, at: usize) -> Option<Found> {
    let covers = |ch: char| is_path_char(ch) || ch == ':';
    if !text[at..].chars().next().is_some_and(covers) {
        return None;
    }
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|&(_, ch)| covers(ch))
        .last()
        .map_or(at, |(index, _)| index);
    let end = text[at..]
        .char_indices()
        .find(|&(_, ch)| !covers(ch))
        .map_or(text.len(), |(index, _)| at + index);
    // Within that, the path is what's between colons that don't begin a
    // line number: `a:b/c.rs:3` is `a` then `b/c.rs:3`.
    let mut pieces = Vec::new();
    let mut piece = start;
    for (index, ch) in text[start..end].char_indices() {
        if ch == ':' && !text[start + index + 1..end].starts_with(|ch: char| ch.is_ascii_digit()) {
            pieces.push(piece..start + index);
            piece = start + index + 1;
        }
    }
    pieces.push(piece..end);
    let piece = pieces
        .into_iter()
        .find(|piece| piece.start <= at && at <= piece.end)?;
    let found = parse_path(&text[piece.clone()])?;
    let bytes = piece.start..piece.start + found.0;
    (bytes.contains(&at)).then_some(Found {
        bytes,
        target: found.1,
    })
}

/// The path at the start of `text`, which holds no colons but before line
/// numbers, with its `:line:col`: how long it is, and where it points.
fn parse_path(text: &str) -> Option<(usize, Target)> {
    let text = trim_end(text);
    let mut parts = text.split(':');
    let path = parts.next()?;
    let mut numbers = parts.map_while(|part| part.parse::<u64>().ok().filter(|&n| n > 0));
    let line = numbers.next();
    let col = line.and_then(|_| numbers.next());
    let path = trim_end(path);
    let looks_like_a_path = path.contains('/')
        || (line.is_some()
            && path.rsplit_once('.').is_some_and(|(name, ext)| {
                !name.is_empty() && !ext.is_empty() && ext.chars().any(char::is_alphabetic)
            }));
    let named = path.chars().any(char::is_alphabetic);
    if !looks_like_a_path || !named || path.contains("//") {
        return None;
    }
    let len = [Some(path.len()), line.map(digits), col.map(digits)]
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, len)| len + usize::from(index > 0))
        .sum();
    Some((
        len,
        Target::File {
            path: path.to_owned(),
            line,
            col,
        },
    ))
}

fn digits(number: u64) -> usize {
    number.to_string().len()
}

/// `text` with its `%XX` escapes decoded.
pub(crate) fn percent_decode(text: &str) -> String {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        let hex = |byte: u8| char::from(byte).to_digit(16);
        if byte == b'%'
            && let [high, low, ..] = after
            && let (Some(high), Some(low)) = (hex(*high), hex(*low))
        {
            bytes.push(u8::try_from(high * 16 + low).expect("two hex digits fit in a byte"));
            rest = &after[2..];
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Target {
        Target::Url(text.to_owned())
    }

    fn file(path: &str, line: Option<u64>, col: Option<u64>) -> Target {
        Target::File {
            path: path.to_owned(),
            line,
            col,
        }
    }

    /// Every link in `text`, as the text it covers and what it points to.
    fn links(text: &str) -> Vec<(&str, Target)> {
        find_all(text)
            .into_iter()
            .map(|found| (&text[found.bytes], found.target))
            .collect()
    }

    #[test]
    fn urls_end_where_the_sentence_or_brackets_around_them_do() {
        let cases: &[(&str, &[&str])] = &[
            ("see https://example.com.", &["https://example.com"]),
            ("(https://example.com/a)", &["https://example.com/a"]),
            ("<https://example.com/a>", &["https://example.com/a"]),
            ("\"https://example.com/a\",", &["https://example.com/a"]),
            ("'http://x.y/z?q=1&r=2#f'", &["http://x.y/z?q=1&r=2#f"]),
            (
                "https://en.wikipedia.org/wiki/Rust_(programming_language)",
                &["https://en.wikipedia.org/wiki/Rust_(programming_language)"],
            ),
            (
                "[docs](https://docs.rs/clap/latest/clap/)",
                &["https://docs.rs/clap/latest/clap/"],
            ),
            ("at https://x.y/a:b, then", &["https://x.y/a:b"]),
            (
                "two: https://a.b/1 https://c.d/2!",
                &["https://a.b/1", "https://c.d/2"],
            ),
            ("HTTPS://EXAMPLE.COM/", &["HTTPS://EXAMPLE.COM/"]),
            (
                "mail mailto:jack@example.com.",
                &["mailto:jack@example.com"],
            ),
            ("xhttps://nope.example", &[]),
            ("https://", &[]),
            ("http: //spaced", &[]),
        ];
        for (text, expected) in cases {
            let found: Vec<&str> = links(text).into_iter().map(|(text, _)| text).collect();
            assert_eq!(found, *expected, "in {text:?}");
        }
        assert_eq!(
            links("go to https://github.com/rust-lang/rust/pull/123 now")[0].1,
            url("https://github.com/rust-lang/rust/pull/123")
        );
    }

    #[test]
    fn file_urls_are_files() {
        assert_eq!(
            links("file:///home/j/My%20Notes.md")[0].1,
            file("/home/j/My Notes.md", None, None)
        );
        assert_eq!(
            target_of_uri("file://localhost/etc/hosts#12"),
            Some(file("/etc/hosts", Some(12), None))
        );
        assert_eq!(
            target_of_uri("file:///etc/hosts#L3"),
            Some(file("/etc/hosts", Some(3), None))
        );
        // Line 0 is no line.
        assert_eq!(
            target_of_uri("file:///etc/hosts#0"),
            Some(file("/etc/hosts", None, None))
        );
        assert_eq!(
            target_of_uri("file://laptop/etc/hosts"),
            Some(Target::Elsewhere {
                uri: "file://laptop/etc/hosts".into(),
                host: "laptop".into(),
            })
        );
        assert_eq!(target_of_uri("file://nohost"), None);
    }

    #[test]
    fn only_known_schemes_open() {
        assert_eq!(target_of_uri("https://a"), Some(url("https://a")));
        assert_eq!(target_of_uri("SSH://host"), Some(url("SSH://host")));
        assert_eq!(target_of_uri("mailto:a@b"), Some(url("mailto:a@b")));
        assert_eq!(target_of_uri("javascript:alert(1)"), None);
        assert_eq!(target_of_uri("-oProxyCommand=x"), None);
        assert_eq!(target_of_uri("vscode://file/x"), None);
        assert_eq!(target_of_uri("https:"), None);
    }

    #[test]
    fn compiler_grep_and_traceback_paths_with_their_lines() {
        let cases: &[(&str, &str, Target)] = &[
            (
                "  --> src/main.rs:12:5",
                "src/main.rs:12:5",
                file("src/main.rs", Some(12), Some(5)),
            ),
            (
                "src/lib.rs:40:    let x = 1;",
                "src/lib.rs:40",
                file("src/lib.rs", Some(40), None),
            ),
            (
                "main.c:3:10: error: expected ';'",
                "main.c:3:10",
                file("main.c", Some(3), Some(10)),
            ),
            (
                "    at Object.<anonymous> (/home/j/app/index.js:7:9)",
                "/home/j/app/index.js:7:9",
                file("/home/j/app/index.js", Some(7), Some(9)),
            ),
            (
                "  File \"/usr/lib/python3/x.py\", line 4",
                "/usr/lib/python3/x.py",
                file("/usr/lib/python3/x.py", None, None),
            ),
            (
                "modified:   crates/gui/src/links.rs",
                "crates/gui/src/links.rs",
                file("crates/gui/src/links.rs", None, None),
            ),
            (
                "see ./build.sh.",
                "./build.sh",
                file("./build.sh", None, None),
            ),
            (
                "cd ~/projects/clackatoo",
                "~/projects/clackatoo",
                file("~/projects/clackatoo", None, None),
            ),
            (
                "../up/one.txt:9",
                "../up/one.txt:9",
                file("../up/one.txt", Some(9), None),
            ),
            (
                "node_modules/@scope/pkg/index.d.ts:1:1",
                "node_modules/@scope/pkg/index.d.ts:1:1",
                file("node_modules/@scope/pkg/index.d.ts", Some(1), Some(1)),
            ),
            ("a/b.rs:0:3", "a/b.rs", file("a/b.rs", None, None)),
        ];
        for (text, covered, target) in cases {
            let found = links(text);
            assert_eq!(found, [(*covered, target.clone())], "in {text:?}");
        }
    }

    #[test]
    fn words_numbers_and_times_are_not_paths() {
        for text in [
            "e.g. this",
            "i.e.",
            "error: expected type, found `u32`",
            "took 12:30",
            "version 1.2.3",
            "3.14:2",
            "warning: unused variable: `x`",
            "Compiling clackatoo v0.1.0",
            "a//b",
        ] {
            assert_eq!(links(text), [], "in {text:?}");
        }
    }

    #[test]
    fn the_link_under_a_point_is_found_from_any_of_its_bytes() {
        let text = "error at src/a.rs:3:1 and https://x.y/z.";
        let path = text.find("src").unwrap();
        for at in path..path + "src/a.rs:3:1".len() {
            assert_eq!(
                find_at(text, at).map(|found| found.target),
                Some(file("src/a.rs", Some(3), Some(1))),
                "at {at}"
            );
        }
        assert_eq!(find_at(text, path - 1), None);
        let url_start = text.find("https").unwrap();
        assert_eq!(
            find_at(text, url_start + 10).map(|found| found.bytes),
            Some(url_start..text.len() - 1)
        );
        assert_eq!(find_at(text, text.len() - 1), None, "the full stop");
    }

    #[test]
    fn a_path_in_a_url_is_the_urls() {
        let text = "https://x.y/src/main.rs:3";
        assert_eq!(
            find_at(text, 15).map(|found| found.target),
            Some(url("https://x.y/src/main.rs:3"))
        );
    }

    #[test]
    fn wide_characters_and_accents_count_in_bytes() {
        let text = "字 ~/notes/café.md:2 字";
        let found = links(text);
        assert_eq!(
            found,
            [("~/notes/café.md:2", file("~/notes/café.md", Some(2), None))]
        );
    }
}
