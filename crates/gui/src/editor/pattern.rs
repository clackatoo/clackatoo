//! Patterns, as Vim writes them, for an editor's `/` and `?`, `:s`, `:g`
//! and the addresses of its ranges.
//!
//! A pattern is translated into the regex crate's syntax, which is
//! ripgrep's engine and what the finder's grep uses, and matched with it.
//! Vim's syntax is translated as far as the engine goes: `magic` by
//! default, with `\v`, `\m`, `\M` and `\V` changing how much is special;
//! `\(`, `\)`, `\%(` and `\|`; `*`, `\+`, `\=`, `\?` and `\{n,m}`, with
//! `\{-n,m}` matching as little as it can; `\<` and `\>`; the classes
//! `\s`, `\d`, `\w`, `\a`, `\l`, `\u`, `\x`, `\o`, `\h`, `\k` and their
//! kin, each with `\_` to take line breaks too; `[]`; `\n`, `\t`, `\e` and
//! `\r`; `~`, the last replacement; `\zs` and `\ze` outside groups; and
//! `\c` and `\C`. What the engine can't do is refused, with why: `\1` in
//! a pattern, `\@` lookaround, `\&` and `\%V`.
//!
//! Words are as Vim's: `\k` is letters, digits and `_`, with what the
//! language adds to them (see [`Words`]), such as CSS's `-`, as
//! `iskeyword` does, and past ASCII, the characters Vim's classes put in
//! words (see [`crate::motion::word_classes`]): not `ª` or `²`, but CJK
//! ideographs and emoji. The engine's own word boundaries know none of
//! that, so a pattern with `\<` or `\>` is matched by fancy-regex
//! instead, which can look behind and ahead: `\<` is a place where a
//! word's character follows one of another class or none, as Vim's
//! regex engine finds it, and `\>` the other way round. fancy-regex
//! backtracks, so it gives up past [`MOST_BACKTRACKS`] steps of one
//! search, rather than hang, and the pattern says so
//! ([`gave_up`](Pattern::gave_up)). Every other pattern is the regex
//! crate's alone.
//!
//! A line break in the text is `\n`, as in Vim, so `^` and `$` are each
//! line's start and end, `.` doesn't take a line break, and nor does a
//! class such as `[^a]` or `\S`.

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::rc::Rc;

use regex::{CaptureLocations, Regex, RegexBuilder};

use crate::motion::Words;
use crate::options::Case;

/// The most memory a compiled pattern takes, so a huge one is refused at
/// once, rather than compiled again at every key of a preview.
const MOST_COMPILED: usize = 1 << 20;

/// The most steps back one search with fancy-regex takes before giving
/// up. It takes about one for each place in the text it tries, from the
/// first the regex crate finds could match, so this is some megabytes of
/// places that nearly match; a pattern that backtracks badly reaches it in
/// a fraction of a second, or in tests, built unoptimised, a smaller one.
pub(crate) const MOST_BACKTRACKS: usize = if cfg!(test) { 500_000 } else { 8_000_000 };

/// What the bar says of a search that gave up.
pub(crate) const TOO_COMPLEX: &str = "too complex a pattern to search this file with";

/// How a pattern's case is matched, unless `\c` or `\C` in it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Matching {
    /// As `ignorecase` and `smartcase` say, as typed patterns are.
    Typed(Case),
    /// As `ignorecase` says, never as `smartcase`: as `*` searches.
    Always(Case),
    /// Exactly, as `:s`'s `I` flag says.
    Exact,
    /// Ignoring case, as `:s`'s `i` flag says.
    Ignore,
}

/// A pattern, ready to match.
#[derive(Clone, Debug)]
pub(crate) struct Pattern {
    engine: Engine,
    /// The regex's group for each of Vim's `\1` to `\9`, by number from 1.
    groups: Vec<usize>,
    /// The group between `\zs` and `\ze`, which is what matches, if the
    /// pattern has either.
    focus: Option<usize>,
    /// Whether it can match a line break, so a match can go on past its
    /// line's end.
    multiline: bool,
    /// Whether a search with it has given up since it was made.
    gave_up: Cell<bool>,
    /// The text it's searching, kept so it's still there, and whether its
    /// words are the regex crate's, so `\<` and `\>` can be its; one for
    /// it and every copy of it, as the editor keeps it and hands it out
    /// for each search (see [`searching`](Self::searching)).
    plain_in: Rc<RefCell<Option<Searching>>>,
}

/// A file's whole text being searched, kept so no other can be where it
/// is, and whether its words are the regex crate's (see
/// [`Pattern::plain_in`]).
type Searching = (Rc<str>, bool);

/// Whether `text` has no character `odd` finds (see [`Pattern::plain_in`]),
/// which goes through it all.
fn plain_words(text: &str, odd: &Regex) -> bool {
    #[cfg(test)]
    PLAIN_SCANS.with(|scans| scans.set(scans.get() + 1));
    !odd.is_match(text)
}

#[cfg(test)]
thread_local! {
    /// How many times [`plain_words`] has gone through a text.
    pub(crate) static PLAIN_SCANS: Cell<usize> = const { Cell::new(0) };
    /// How many searches [`Pattern::find_at`] has made.
    pub(crate) static FINDS: Cell<usize> = const { Cell::new(0) };
}

/// What matches a pattern.
#[derive(Clone, Debug)]
enum Engine {
    /// The regex crate, for every pattern but those below.
    Plain(Regex),
    /// fancy-regex, for a pattern with `\<` or `\>`, whose words the
    /// regex crate's word boundaries don't know; with the
    /// regex crate's for the same pattern without them, which matches
    /// wherever it does and more, so a search starts no sooner than where
    /// that finds a match, and none is made where it finds none.
    Looking {
        regex: fancy_regex::Regex,
        loose: Regex,
        /// The pattern with the regex crate's own word boundaries, which
        /// find the same matches as `regex` in a text without `odd` in
        /// it, and never give up: as minified code, a line of megabytes,
        /// would make `regex` do.
        bounded: Box<Regex>,
        /// A character the regex crate's words and Vim's disagree about,
        /// or of a script's own class.
        odd: Box<Regex>,
    },
}

/// Where a match's groups are, as [`Pattern::captures_at`] keeps them.
#[derive(Clone, Debug)]
pub(crate) enum Locations {
    Plain(CaptureLocations),
    Looking(Vec<Option<(usize, usize)>>),
}

impl Locations {
    /// Where group `index` of the regex matched, if it did.
    fn get(&self, index: usize) -> Option<(usize, usize)> {
        match self {
            Self::Plain(locations) => locations.get(index),
            Self::Looking(locations) => locations.get(index).copied().flatten(),
        }
    }
}

/// Where a match is, in bytes of the text: what Vim calls the match, which
/// `\zs` and `\ze` narrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl Pattern {
    /// `source`, Vim's pattern, with case matched as `matching` says, `~`
    /// standing for `tilde`, the last replacement, and `words` part of
    /// words. `Err` says why it can't be.
    pub(crate) fn new(
        source: &str,
        matching: Matching,
        tilde: &str,
        words: Words,
    ) -> Result<Self, String> {
        let translated = translate(source, tilde, words)?;
        let ignore = match translated.case {
            Some(exact) => !exact,
            None => match matching {
                Matching::Typed(case) => !case.exact(&capitals(source)),
                Matching::Always(case) => !case.exact_always(),
                Matching::Exact => false,
                Matching::Ignore => true,
            },
        };
        let cant = || format!("{source} isn't a pattern this editor can match");
        let plain = |regex: &str| {
            RegexBuilder::new(regex)
                .multi_line(true)
                .case_insensitive(ignore)
                .size_limit(MOST_COMPILED)
                .build()
                .map_err(|err| match err {
                    regex::Error::CompiledTooBig(_) => format!("{source} is too big a pattern"),
                    _ => cant(),
                })
        };
        let engine = if translated.looks {
            let loose = (translated.regex)
                .replace(&edge(words, true), "")
                .replace(&edge(words, false), "");
            let regex = fancy_regex::RegexBuilder::new(&translated.regex)
                .multi_line(true)
                .case_insensitive(ignore)
                .delegate_size_limit(MOST_COMPILED)
                .backtrack_limit(MOST_BACKTRACKS)
                .build()
                .map_err(|err| match err {
                    fancy_regex::Error::CompileError(_) => format!("{source} is too big a pattern"),
                    _ => cant(),
                })?;
            let bounded = (translated.regex)
                .replace(&edge(words, true), r"\b{start}")
                .replace(&edge(words, false), r"\b{end}");
            let classes = word_classes_inside(words);
            let odd = format!(
                r"[[\w--[{words}]][[{words}]--\w][{scripts}]]",
                words = classes[0],
                scripts = classes[1..].concat(),
            );
            Engine::Looking {
                regex,
                loose: plain(&loose)?,
                bounded: Box::new(plain(&bounded)?),
                odd: Box::new(Regex::new(&odd).map_err(|_| cant())?),
            }
        } else {
            Engine::Plain(plain(&translated.regex)?)
        };
        Ok(Self {
            engine,
            groups: translated.groups,
            focus: translated.focus,
            multiline: translated.multiline,
            gave_up: Cell::new(false),
            plain_in: Rc::default(),
        })
    }

    /// Whether it can match a line break.
    pub(crate) fn multiline(&self) -> bool {
        self.multiline
    }

    /// Whether a search with it gave up, as one that backtracks too much
    /// does, and found nothing for that, since it was made: so the bar
    /// says it's [`TOO_COMPLEX`], not that it isn't in the file.
    pub(crate) fn gave_up(&self) -> bool {
        self.gave_up.get()
    }

    /// What the bar says of a search with it that found nothing: that it
    /// gave up, or else `why`.
    pub(crate) fn not_found(&self, why: String) -> String {
        if self.gave_up() {
            TOO_COMPLEX.to_owned()
        } else {
            why
        }
    }

    /// Room for where a match's groups are, for
    /// [`captures_at`](Self::captures_at).
    pub(crate) fn locations(&self) -> Locations {
        match &self.engine {
            Engine::Plain(regex) => Locations::Plain(regex.capture_locations()),
            Engine::Looking { .. } => Locations::Looking(Vec::new()),
        }
    }

    /// The first match in `text` tried from byte `at`, keeping where its
    /// groups are in `locations`.
    pub(crate) fn captures_at(
        &self,
        text: &str,
        at: usize,
        locations: &mut Locations,
    ) -> Option<Found> {
        match (&self.engine, &mut *locations) {
            (Engine::Plain(regex), Locations::Plain(kept)) => {
                regex.captures_read_at(kept, text, at)?;
            }
            (Engine::Looking { bounded, odd, .. }, kept) if self.plain_in(text, odd) => {
                let mut found = bounded.capture_locations();
                bounded.captures_read_at(&mut found, text, at)?;
                *kept =
                    Locations::Looking((0..found.len()).map(|index| found.get(index)).collect());
            }
            (Engine::Looking { regex, loose, .. }, kept) => {
                *kept = Locations::Looking(self.looking(loose, text, at, |hay, at| {
                    let captures = regex.captures_from_pos(hay, at).map_err(Box::new)?;
                    Ok(captures.map(|captures| {
                        (0..captures.len())
                            .map(|index| captures.get(index).map(|got| (got.start(), got.end())))
                            .collect()
                    }))
                })?);
            }
            (Engine::Plain(_), kept) => {
                *kept = self.locations();
                return self.captures_at(text, at, kept);
            }
        }
        let (start, end) = locations.get(self.focus.unwrap_or(0))?;
        Some(Found { start, end })
    }

    /// Makes `whole` the text it searches, a file's whole text, and works
    /// out whether its words are the regex crate's, once for it and its
    /// copies: as the editor makes a file's text one string anew after
    /// each edit, which it hands to every search till the next.
    pub(crate) fn searching(&self, whole: &Rc<str>) {
        let Engine::Looking { odd, .. } = &self.engine else {
            return;
        };
        let mut kept = self.plain_in.borrow_mut();
        if kept
            .as_ref()
            .is_none_or(|(text, _)| !Rc::ptr_eq(text, whole))
        {
            *kept = Some((Rc::clone(whole), plain_words(whole, odd)));
        }
    }

    /// Whether `text`'s words are the regex crate's: it has no character
    /// `odd` finds, where Vim's words and the crate's differ, so the
    /// crate's word boundaries are Vim's. Known for the text it's
    /// searching, or part of it, which it keeps so no other can be where
    /// it is (see [`searching`](Self::searching)); for any other, worked
    /// out each time. A part of a text that isn't plain may be, but the
    /// look-arounds are right for it all the same.
    fn plain_in(&self, text: &str, odd: &Regex) -> bool {
        if let Some((whole, plain)) = self.plain_in.borrow().as_ref() {
            let (start, end) = (
                whole.as_ptr() as usize,
                whole.as_ptr() as usize + whole.len(),
            );
            let at = text.as_ptr() as usize;
            if start <= at && at + text.len() <= end {
                return *plain;
            }
        }
        plain_words(text, odd)
    }

    /// The first match `search` finds in `text` from byte `at`, as
    /// fancy-regex does, starting from where `loose`, the regex crate's
    /// pattern without the look-arounds, finds one could be. For a pattern
    /// that can't cross a line break, `search` is given the text only to
    /// the end of that line, and if it finds nothing there, the next is
    /// looked for after it: so how many steps back a search takes before
    /// giving up is counted a line at a time, not through the whole text.
    /// A search that gives up finds nothing, and is noted.
    fn looking<T>(
        &self,
        loose: &Regex,
        text: &str,
        at: usize,
        mut search: impl FnMut(&str, usize) -> Result<Option<T>, Box<fancy_regex::Error>>,
    ) -> Option<T> {
        let mut at = at;
        loop {
            let start = loose.find_at(text, at)?.start();
            let end = if self.multiline {
                text.len()
            } else {
                (text[start..].find('\n')).map_or(text.len(), |line| start + line)
            };
            match search(&text[..end], start) {
                Ok(Some(found)) => return Some(found),
                Ok(None) if end < text.len() => at = end + 1,
                Ok(None) => return None,
                Err(err) => {
                    log::debug!("a search gave up: {err}");
                    self.gave_up.set(true);
                    return None;
                }
            }
        }
    }

    /// The first match in `text` tried from byte `at`.
    pub(crate) fn find_at(&self, text: &str, at: usize) -> Option<Found> {
        #[cfg(test)]
        FINDS.with(|finds| finds.set(finds.get() + 1));
        if self.focus.is_some() {
            return self.captures_at(text, at, &mut self.locations());
        }
        let (start, end) = match &self.engine {
            Engine::Plain(regex) => {
                let found = regex.find_at(text, at)?;
                (found.start(), found.end())
            }
            Engine::Looking { bounded, odd, .. } if self.plain_in(text, odd) => {
                let found = bounded.find_at(text, at)?;
                (found.start(), found.end())
            }
            Engine::Looking { regex, loose, .. } => self.looking(loose, text, at, |hay, at| {
                let found = regex.find_from_pos(hay, at).map_err(Box::new)?;
                Ok(found.map(|found| (found.start(), found.end())))
            })?,
        };
        Some(Found { start, end })
    }

    /// Where Vim's group `number` matched, as last kept in `locations`:
    /// 0 is the whole match.
    pub(crate) fn group(&self, locations: &Locations, number: usize) -> Option<Found> {
        let index = match number {
            0 => self.focus.unwrap_or(0),
            _ => *self.groups.get(number - 1)?,
        };
        let (start, end) = locations.get(index)?;
        Some(Found { start, end })
    }
}

/// `source` with what follows a backslash left out, which isn't counted
/// as a capital by `smartcase`, as in Vim: `\S` is a class, not an `S`.
fn capitals(source: &str) -> String {
    let mut kept = String::with_capacity(source.len());
    let mut chars = source.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            // `\_x` and `\%x` are one escape.
            if matches!(chars.next(), Some('_' | '%')) {
                chars.next();
            }
            continue;
        }
        kept.push(ch);
    }
    kept
}

/// Escapes `text`, so a pattern matches it as it is, in Vim's `magic`.
pub(crate) fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '/' | '.' | '*' | '$' | '^' | '~' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// A pattern translated into the regex crate's syntax.
#[derive(Debug)]
struct Translated {
    regex: String,
    groups: Vec<usize>,
    focus: Option<usize>,
    /// Whether it looks behind or ahead, as `\<` and `\>` do where words
    /// have more in them than the regex crate knows: see the module's
    /// docs.
    looks: bool,
    /// Whether `\C` (`true`) or `\c` said how to match case.
    case: Option<bool>,
    multiline: bool,
}

/// How much of a pattern is special without a backslash, as `\v`, `\m`,
/// `\M` and `\V` say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Magic {
    Very,
    On,
    Off,
    VeryOff,
}

/// One piece of a pattern, read with its backslash, if it has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Token {
    ch: char,
    /// Whether it's special, as the magic says of it with or without its
    /// backslash.
    special: bool,
    /// Whether it had a backslash.
    escaped: bool,
}

/// Whether `ch`, with a backslash if `escaped`, is special in `magic`.
/// Letters and digits after a backslash are escapes of their own, read
/// apart.
fn is_special(ch: char, escaped: bool, magic: Magic) -> bool {
    const ALWAYS: &str = "^$";
    const MAGIC: &str = ".*[~";
    match magic {
        Magic::Very => {
            !escaped && !ch.is_ascii_alphanumeric() && ch != '_' && ch.is_ascii_punctuation()
        }
        Magic::On => {
            if escaped {
                !ALWAYS.contains(ch) && !MAGIC.contains(ch)
            } else {
                ALWAYS.contains(ch) || MAGIC.contains(ch)
            }
        }
        Magic::Off => {
            if escaped {
                !ALWAYS.contains(ch)
            } else {
                ALWAYS.contains(ch)
            }
        }
        Magic::VeryOff => escaped,
    }
}

/// A class of characters, as `\s` or `\d` names one: what's in it, as the
/// inside of a regex class, and whether that's what's left out.
/// Capitals and small letters, `\u`, `\l` and their opposites, match case
/// whatever `ignorecase` says, as in Vim (see [`class_atom`]).
fn class(ch: char) -> Option<(&'static str, bool)> {
    Some(match ch {
        's' => (r" \t", false),
        'S' => (r" \t", true),
        'd' => ("0-9", false),
        'D' => ("0-9", true),
        'w' => ("0-9A-Za-z_", false),
        'W' => ("0-9A-Za-z_", true),
        'a' => ("A-Za-z", false),
        'A' => ("A-Za-z", true),
        'l' => ("a-z", false),
        'L' => ("a-z", true),
        'u' => ("A-Z", false),
        'U' => ("A-Z", true),
        'x' => ("0-9A-Fa-f", false),
        'X' => ("0-9A-Fa-f", true),
        'o' => ("0-7", false),
        'O' => ("0-7", true),
        'h' => ("A-Za-z_", false),
        'H' => ("A-Za-z_", true),
        // Keywords and identifiers, as `iskeyword`'s default reads them,
        // with letters beyond ASCII; then without digits.
        'k' | 'i' => (r"\w", false),
        'K' | 'I' => (r"\w&&[^0-9]", false),
        // File names, as `isfname`'s default reads them.
        'f' => (r"\w/.\-+,#$%~=", false),
        'F' => (r"\w/.\-+,#$%~=&&[^0-9]", false),
        // Printable characters.
        'p' => (r"^\p{Cc}", false),
        'P' => (r"^\p{Cc}0-9", false),
        _ => return None,
    })
}

/// A class as a regex, taking line breaks too if `breaks`, as `\_` says.
/// One that leaves characters out never takes a line break unless told.
/// A line break is an alternative of its own, as the inside can be an
/// intersection (`\w&&[^0-9]`), which a `\n` added to it would join.
fn class_regex(inside: &str, negated: bool, breaks: bool) -> String {
    match (negated, breaks) {
        (false, false) => format!("[{inside}]"),
        (false, true) => format!(r"(?:[{inside}]|\n)"),
        (true, false) => format!(r"[^{inside}\n]"),
        (true, true) => format!("[^{inside}]"),
    }
}

/// The class `\ch` names, as a regex, taking line breaks too if `breaks`.
fn class_atom(ch: char, breaks: bool) -> Option<String> {
    let (inside, negated) = class(ch)?;
    let regex = class_regex(inside, negated, breaks);
    // Case matters to these, whatever `ignorecase` says.
    Some(if matches!(ch, 'u' | 'U' | 'l' | 'L') {
        format!("(?-i:{regex})")
    } else {
        regex
    })
}

/// The reader of a pattern, writing the regex as it goes.
// What's been read, not states of one machine.
#[allow(clippy::struct_excessive_bools)]
struct Reader<'a> {
    source: &'a str,
    rest: std::str::Chars<'a>,
    magic: Magic,
    out: String,
    /// The regex's group number of each of Vim's groups so far, and where
    /// in `out` each opened.
    groups: Vec<(usize, usize)>,
    /// How many groups the regex has opened.
    opened: usize,
    /// What words are made of besides letters, digits and `_`.
    words: Words,
    /// Whether it has written a look behind or ahead.
    looks: bool,
    /// How deep in groups the reader is.
    depth: usize,
    /// Where in `out` `\zs` and `\ze` were, at the top level.
    zs: Option<usize>,
    ze: Option<usize>,
    /// Whether the top level has a `\|`.
    alternatives: bool,
    case: Option<bool>,
    multiline: bool,
    /// Whether what's been read so far could be followed by `^` as the
    /// line's start: nothing, or a group's start or `\|`.
    at_start: bool,
    /// Whether the last thing written can't take a `*`, which is then
    /// itself: the pattern's start, or `^`.
    no_repeat: bool,
}

/// Translates `source`, with `~` standing for `tilde`, and `words` part
/// of words.
fn translate(source: &str, tilde: &str, words: Words) -> Result<Translated, String> {
    let mut reader = Reader {
        source,
        rest: source.chars(),
        magic: Magic::On,
        out: String::with_capacity(source.len() * 2),
        groups: Vec::new(),
        opened: 0,
        words,
        looks: false,
        depth: 0,
        zs: None,
        ze: None,
        alternatives: false,
        case: None,
        multiline: false,
        at_start: true,
        no_repeat: true,
    };
    while let Some(token) = reader.token() {
        reader.read(token, tilde)?;
    }
    if reader.depth > 0 {
        return Err(format!("{source} opens a group it doesn't close"));
    }
    reader.finish()
}

impl Reader<'_> {
    /// The next piece of the pattern, if there's more.
    fn token(&mut self) -> Option<Token> {
        let ch = self.rest.next()?;
        if ch == '\\' {
            let Some(next) = self.rest.next() else {
                // A backslash at the end is itself.
                return Some(Token {
                    ch: '\\',
                    special: false,
                    escaped: true,
                });
            };
            return Some(Token {
                ch: next,
                special: !next.is_ascii_alphanumeric()
                    && next != '_'
                    && is_special(next, true, self.magic),
                escaped: true,
            });
        }
        // In `\V`, `^` is the line's start at the pattern's start, and `$`
        // its end at the end, as in Vim.
        let anchor = self.magic == Magic::VeryOff
            && ((ch == '^' && self.at_start) || (ch == '$' && self.dollar_ends()));
        Some(Token {
            ch,
            special: anchor || is_special(ch, false, self.magic),
            escaped: false,
        })
    }

    /// What's left of the pattern, unread.
    fn peek(&self) -> Option<char> {
        self.rest.clone().next()
    }

    /// Whether what's left starts with a `$` that ends the pattern or a
    /// branch of it, so is the line's end.
    fn dollar_ends(&self) -> bool {
        let rest = self.rest.as_str();
        if rest.is_empty() {
            return true;
        }
        let bare = |text: &str| match self.magic {
            Magic::Very => rest.starts_with(text),
            _ => rest
                .strip_prefix('\\')
                .is_some_and(|rest| rest.starts_with(text)),
        };
        bare("|") || bare(")") || rest.starts_with("\\n")
    }

    fn literal(&mut self, ch: char) {
        let mut buffer = [0; 4];
        self.out
            .push_str(&regex::escape(ch.encode_utf8(&mut buffer)));
        self.wrote();
    }

    /// Notes that an atom was written, which can take a `*`.
    fn wrote(&mut self) {
        self.at_start = false;
        self.no_repeat = false;
    }

    fn read(&mut self, token: Token, tilde: &str) -> Result<(), String> {
        let Token {
            ch,
            special,
            escaped,
        } = token;
        if escaped && (ch.is_ascii_alphanumeric() || ch == '_') {
            return self.escape(ch);
        }
        if !special {
            self.literal(ch);
            return Ok(());
        }
        match ch {
            '^' => {
                if self.at_start {
                    self.out.push('^');
                    self.no_repeat = true;
                    self.at_start = false;
                } else {
                    self.literal('^');
                }
            }
            '$' => {
                if self.dollar_ends() {
                    self.out.push('$');
                    self.wrote();
                } else {
                    self.literal('$');
                }
            }
            '.' => {
                self.out.push('.');
                self.wrote();
            }
            '*' | '+' | '=' | '?' if self.no_repeat => self.literal(ch),
            '*' => self.out.push('*'),
            '+' => self.out.push('+'),
            '=' | '?' => self.out.push('?'),
            '{' => self.interval()?,
            '[' => self.bracket(false),
            '~' => {
                // As one atom, so `~*` repeats all of it.
                self.out.push_str("(?:");
                self.out.push_str(&regex::escape(tilde));
                self.out.push(')');
                self.wrote();
            }
            '(' => self.open(true),
            ')' => {
                if self.depth == 0 {
                    return Err(format!("{} closes a group it didn't open", self.source));
                }
                self.depth -= 1;
                self.out.push(')');
                self.wrote();
            }
            '|' => {
                if self.depth == 0 {
                    self.alternatives = true;
                }
                self.out.push('|');
                self.at_start = true;
                self.no_repeat = true;
            }
            '<' | '>' => {
                // See the module's docs.
                self.out.push_str(&edge(self.words, ch == '<'));
                self.looks = true;
                self.no_repeat = true;
            }
            '%' => self.percent()?,
            '@' => return Err(format!("{}: \\@ lookaround isn't supported", self.source)),
            '&' => return Err(format!("{}: \\& isn't supported", self.source)),
            _ => self.literal(ch),
        }
        Ok(())
    }

    /// Opens a group, which `\1` and its kin can name if `capturing`.
    fn open(&mut self, capturing: bool) {
        if capturing {
            self.opened += 1;
            self.groups.push((self.opened, self.out.len()));
            self.out.push('(');
        } else {
            self.out.push_str("(?:");
        }
        self.depth += 1;
        self.at_start = true;
        self.no_repeat = true;
    }

    /// A letter or digit after a backslash.
    fn escape(&mut self, ch: char) -> Result<(), String> {
        match ch {
            'n' => {
                self.out.push_str(r"\n");
                self.multiline = true;
                self.wrote();
                self.at_start = true;
            }
            't' => self.push_atom(r"\t"),
            'e' => self.push_atom(r"\x1B"),
            'r' => self.push_atom(r"\r"),
            'b' => self.push_atom(r"\x08"),
            'v' => self.magic = Magic::Very,
            'm' => self.magic = Magic::On,
            'M' => self.magic = Magic::Off,
            'V' => self.magic = Magic::VeryOff,
            'c' => self.case = Some(false),
            'C' => self.case = Some(true),
            // Combining characters are matched as they are.
            'Z' => {}
            'z' => match self.rest.next() {
                Some('s') => self.focus_mark(true)?,
                Some('e') => self.focus_mark(false)?,
                _ => {
                    return Err(format!(
                        "{}: \\z isn't supported but as \\zs and \\ze",
                        self.source
                    ));
                }
            },
            '_' => self.underscore()?,
            '1'..='9' => {
                return Err(format!(
                    "{}: \\{ch} in a pattern isn't supported, only in a replacement",
                    self.source
                ));
            }
            _ => match self.class_atom(ch, false) {
                Some(regex) => self.push_atom(&regex),
                None => {
                    return Err(format!("{}: \\{ch} isn't supported", self.source));
                }
            },
        }
        Ok(())
    }

    /// The class `\ch` names, as [`class_atom`] says, but for `\k` and
    /// `\K`, which take what words have in them besides letters, digits
    /// and `_`.
    fn class_atom(&self, ch: char, breaks: bool) -> Option<String> {
        if !matches!(ch, 'k' | 'K') {
            return class_atom(ch, breaks);
        }
        let mut inside = keyword_inside(self.words);
        if ch == 'K' {
            inside.push_str("&&[^0-9]");
        }
        Some(class_regex(&inside, false, breaks))
    }

    fn push_atom(&mut self, regex: &str) {
        self.out.push_str(regex);
        self.wrote();
    }

    /// `\zs`, if `start`, or `\ze`: where what matches starts or ends.
    fn focus_mark(&mut self, start: bool) -> Result<(), String> {
        if self.depth > 0 {
            return Err(format!(
                "{}: \\zs and \\ze are supported only outside groups",
                self.source
            ));
        }
        let at = Some(self.out.len());
        if start {
            self.zs = at;
        } else {
            self.ze = at;
        }
        self.no_repeat = true;
        Ok(())
    }

    /// `\_` and what follows: a class, or `.`, `^`, `$` or `[`, taking
    /// line breaks too.
    fn underscore(&mut self) -> Result<(), String> {
        let Some(ch) = self.rest.next() else {
            return Err(format!("{}: \\_ needs a class after it", self.source));
        };
        self.multiline = true;
        match ch {
            '.' => self.push_atom(r"(?s:.)"),
            '^' => {
                self.out.push('^');
                self.no_repeat = true;
            }
            '$' => self.push_atom("$"),
            '[' => self.bracket(true),
            _ => match self.class_atom(ch, true) {
                Some(regex) => self.push_atom(&regex),
                None => return Err(format!("{}: \\_{ch} isn't supported", self.source)),
            },
        }
        Ok(())
    }

    /// `\{` and what follows, to `}`: how many times the last atom goes.
    fn interval(&mut self) -> Result<(), String> {
        let rest = self.rest.as_str();
        let Some(end) = rest.find('}') else {
            return Err(format!("{}: \\{{ isn't closed with }}", self.source));
        };
        let inside = rest[..end].strip_suffix('\\').unwrap_or(&rest[..end]);
        for _ in rest[..=end].chars() {
            self.rest.next();
        }
        let (lazy, inside) = match inside.strip_prefix('-') {
            Some(inside) => (true, inside),
            None => (false, inside),
        };
        let number = |text: &str| -> Result<Option<u32>, String> {
            if text.is_empty() {
                return Ok(None);
            }
            (text.parse().map(Some))
                .map_err(|_| format!("{}: \\{{{inside}}} isn't a count", self.source))
        };
        let repeat = match inside.split_once(',') {
            None => match number(inside)? {
                None => "*".to_owned(),
                Some(times) => format!("{{{times}}}"),
            },
            Some((least, most)) => match (number(least)?, number(most)?) {
                (None, None) => "*".to_owned(),
                (least, None) => format!("{{{},}}", least.unwrap_or(0)),
                (least, Some(most)) => format!("{{{},{most}}}", least.unwrap_or(0)),
            },
        };
        if self.no_repeat {
            return Err(format!("{}: \\{{ follows nothing to repeat", self.source));
        }
        self.out.push_str(&repeat);
        if lazy {
            self.out.push('?');
        }
        Ok(())
    }

    /// `\%` and what follows.
    fn percent(&mut self) -> Result<(), String> {
        match self.rest.next() {
            Some('(') => self.open(false),
            // The file's start and end: a match can't be found a line at
            // a time.
            Some('^') => {
                self.multiline = true;
                self.push_atom(r"\A");
            }
            Some('$') => {
                self.multiline = true;
                self.push_atom(r"\z");
            }
            Some(base @ ('d' | 'o' | 'x' | 'u' | 'U')) => {
                let (radix, most) = match base {
                    'd' => (10, 10),
                    'o' => (8, 11),
                    'x' => (16, 2),
                    'u' => (16, 4),
                    _ => (16, 8),
                };
                let mut digits = String::new();
                while digits.len() < most
                    && let Some(digit) = self.peek().filter(|ch| ch.is_digit(radix))
                {
                    digits.push(digit);
                    self.rest.next();
                }
                let ch = (u32::from_str_radix(&digits, radix).ok())
                    .and_then(char::from_u32)
                    .ok_or_else(|| {
                        format!("{}: \\%{base}{digits} isn't a character", self.source)
                    })?;
                self.literal(ch);
            }
            Some(ch) => return Err(format!("{}: \\%{ch} isn't supported", self.source)),
            None => return Err(format!("{}: \\% needs something after it", self.source)),
        }
        Ok(())
    }

    /// `[` and what follows, to its `]`, as a class, taking line breaks
    /// too if `breaks`. Without a `]` to close it, it's itself.
    fn bracket(&mut self, breaks: bool) {
        let rest = self.rest.as_str();
        let Some((inside, len)) = bracket_end(rest) else {
            self.literal('[');
            return;
        };
        for _ in rest[..len].chars() {
            self.rest.next();
        }
        let (negated, inside) = match inside.strip_prefix('^') {
            Some(inside) => (true, inside),
            None => (false, inside),
        };
        let mut class = String::with_capacity(inside.len() * 2);
        let mut chars = inside.chars().peekable();
        let mut first = true;
        while let Some(ch) = chars.next() {
            match ch {
                '[' if chars.peek() == Some(&':') => {
                    // A named class, `[:alpha:]`, which the engine reads.
                    let mut name = String::from("[:");
                    chars.next();
                    for ch in chars.by_ref() {
                        name.push(ch);
                        if ch == ']' {
                            break;
                        }
                    }
                    class.push_str(&name);
                }
                '-' if !first && chars.peek().is_some() => class.push('-'),
                '\\' => {
                    let escaped = match chars.next() {
                        Some('e') => '\u{1b}',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some('b') => '\u{8}',
                        Some('n') => '\n',
                        Some(ch @ ('\\' | ']' | '^' | '-')) => ch,
                        Some(base @ ('d' | 'o' | 'x' | 'u' | 'U'))
                            if let Some(ch) = numbered(base, &mut chars) =>
                        {
                            ch
                        }
                        Some(other) => {
                            // Any other is itself, backslash and all.
                            class.push_str(r"\\");
                            other
                        }
                        None => '\\',
                    };
                    push_class_char(&mut class, escaped);
                }
                _ => push_class_char(&mut class, ch),
            }
            first = false;
        }
        if class.is_empty() {
            self.literal('[');
            return;
        }
        let regex = class_regex(&class, negated, breaks);
        self.push_atom(&regex);
    }

    /// The regex, with `\zs` and `\ze` making a group of what's between
    /// them.
    fn finish(self) -> Result<Translated, String> {
        let Self {
            mut out,
            groups,
            looks,
            zs,
            ze,
            alternatives,
            case,
            multiline,
            source,
            ..
        } = self;
        if (zs.is_some() || ze.is_some()) && alternatives {
            return Err(format!(
                "{source}: \\zs and \\ze aren't supported with \\| outside a group"
            ));
        }
        let mut focus = None;
        // The groups opened from where the focus opens come after it.
        let mut start = usize::MAX;
        if zs.is_some() || ze.is_some() {
            start = zs.unwrap_or(0);
            let end = ze.unwrap_or(out.len()).max(start);
            out.insert(end, ')');
            out.insert(start, '(');
            focus = Some(groups.iter().filter(|&&(_, at)| at < start).count() + 1);
        }
        let groups = (groups.into_iter())
            .map(|(number, at)| if at >= start { number + 1 } else { number })
            .collect();
        Ok(Translated {
            regex: out,
            groups,
            focus,
            looks,
            case,
            multiline,
        })
    }
}

/// The character `\d123`, `\o40`, `\x20`, `\u20AC` or `\U...` in a class
/// names, its digits read from `chars`, if they make one. Without digits,
/// nothing is read, and it's itself.
fn numbered(base: char, chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<char> {
    let (radix, most) = match base {
        'd' => (10, 10),
        'o' => (8, 11),
        'x' => (16, 2),
        'u' => (16, 4),
        _ => (16, 8),
    };
    let mut digits = String::new();
    while digits.len() < most
        && let Some(&digit) = chars.peek()
        && digit.is_digit(radix)
    {
        digits.push(digit);
        chars.next();
    }
    if digits.is_empty() {
        return None;
    }
    u32::from_str_radix(&digits, radix)
        .ok()
        .and_then(char::from_u32)
}

/// The inside of a class of the characters of words: `\w`, and `words`.
fn keyword_inside(words: Words) -> String {
    let mut inside = String::new();
    for class in &word_classes_inside(words) {
        inside.push_str(class);
    }
    inside
}

/// The insides of the classes of characters words are made of, as Vim
/// sorts them (see [`crate::motion::word_classes`]): letters, digits, `_`
/// and `words`, with the others of class 2, then each script's own.
fn word_classes_inside(words: Words) -> Vec<String> {
    let classes = crate::motion::word_classes();
    (classes.iter().enumerate())
        .map(|(index, ranges)| {
            let mut inside = String::new();
            if index == 0 {
                inside.push_str("0-9A-Za-z_");
                for word in words.chars() {
                    push_class_char(&mut inside, word);
                }
            }
            for (first, last) in ranges {
                let _ = write!(inside, r"\x{{{first:x}}}-\x{{{last:x}}}");
            }
            inside
        })
        .collect()
}

/// `\<`, if `start`, or else `\>`, as fancy-regex looks for them, as Vim's
/// regex engine finds them: where a word's character follows one of
/// another class, or none, or the other way round; the classes as
/// [`word_classes_inside`] has them, so `中` and `x` are words apart. The
/// character it's at, or after, is looked at first, and the scripts' own
/// classes only if it's in one of them, as few are, so most places are
/// ruled in or out in a step or two.
fn edge(words: Words, start: bool) -> String {
    let classes = word_classes_inside(words);
    let one = |inside: &str| {
        let class = format!("[{inside}]");
        if start {
            format!("(?={class})(?<!{class})")
        } else {
            format!("(?<={class})(?!{class})")
        }
    };
    let scripts: String = classes[1..].concat();
    let each: Vec<String> = classes[1..].iter().map(|inside| one(inside)).collect();
    let at_script = if start {
        format!("(?=[{scripts}])")
    } else {
        format!("(?<=[{scripts}])")
    };
    format!("(?:{}|{at_script}(?:{}))", one(&classes[0]), each.join("|"))
}

/// Adds `ch` to a class being written, escaped as the engine needs.
fn push_class_char(class: &mut String, ch: char) {
    let mut buffer = [0; 4];
    class.push_str(&regex::escape(ch.encode_utf8(&mut buffer)));
}

/// The inside of the class that starts `rest`, just after its `[`, and how
/// many bytes it takes with its `]`, if it's closed. A `]` first, or after
/// `^`, is itself.
fn bracket_end(rest: &str) -> Option<(&str, usize)> {
    let bytes = rest.as_bytes();
    let mut at = 0;
    if bytes.first() == Some(&b'^') {
        at += 1;
    }
    if bytes.get(at) == Some(&b']') {
        at += 1;
    }
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'[' if bytes.get(at + 1) == Some(&b':') => {
                let close = rest[at..].find(":]")?;
                at += close + 2;
            }
            b']' => return Some((&rest[..at], at + 1)),
            _ => at += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regex(source: &str) -> String {
        translate(source, "TILDE", Words::NONE).unwrap().regex
    }

    /// Every match of `source` in `text`, as Vim would find them.
    fn matches(source: &str, text: &str) -> Vec<String> {
        matches_in(source, text, Words::NONE)
    }

    /// How long finding every match takes, in texts that once took
    /// seconds, and in plain ones; and how long a search takes to give up.
    #[test]
    #[ignore = "a benchmark: run with --ignored --nocapture --release"]
    fn word_edge_timings() {
        let dashes = "-x".repeat(50_000);
        // A minified line with an `o` in many words, but starting only the
        // last.
        let css = "a{color:red;border-top:0;font:bold}".repeat(2_300) + "o";
        let big = "a{color:red;border-top:0;font:bold}\n".repeat(110_000);
        let dash = Words::new("-");
        for (source, text, words) in [
            ("\\<x\\k*", dashes.as_str(), dash),
            ("\\<o.*", css.as_str(), dash),
            ("\\<color\\>", css.as_str(), dash),
            ("\\<color\\>", css.as_str(), Words::NONE),
            ("border-top", css.as_str(), dash),
            ("\\<zzz\\>", big.as_str(), dash),
            ("\\<zzz\\>", big.as_str(), Words::NONE),
            ("\\(a*\\)*b\\>", &("a".repeat(40) + "b-"), dash),
        ] {
            let start = std::time::Instant::now();
            let found = matches_in(source, text, words).len();
            let words = if words.is_none() { "plain" } else { "-" };
            eprintln!(
                "{source:12} {words:5} {} KB: {found} matches in {:?}",
                text.len() / 1000,
                start.elapsed()
            );
        }
    }

    /// Every match of `source` in `text`, as Vim would find them with
    /// `words` part of words.
    fn matches_in(source: &str, text: &str, words: Words) -> Vec<String> {
        let pattern = Pattern::new(source, Matching::Exact, "TILDE", words).unwrap();
        let mut found = Vec::new();
        let mut at = 0;
        while at <= text.len()
            && let Some(one) = pattern.find_at(text, at)
        {
            found.push(text[one.start..one.end].to_owned());
            at = if one.end > one.start {
                one.end
            } else {
                one.end + 1
            };
        }
        found
    }

    #[test]
    fn magic_is_vims_default() {
        assert_eq!(matches("a.c", "abc a.c"), ["abc", "a.c"]);
        assert_eq!(matches("a\\.c", "abc a.c"), ["a.c"]);
        // Literal in `magic`, special with a backslash.
        assert_eq!(matches("a+", "aa a+"), ["a+"]);
        assert_eq!(matches("a\\+", "aa a+"), ["aa", "a"]);
        assert_eq!(matches("(c)", "(c) c"), ["(c)"]);
        assert_eq!(matches("\\(c\\)", "(c) c"), ["c", "c"]);
        assert_eq!(matches("{d}|e", "{d}|e"), ["{d}|e"]);
        assert_eq!(matches("a\\|b", "abc"), ["a", "b"]);
        assert_eq!(matches("ab\\=c", "ac abc abbc"), ["ac", "abc"]);
        assert_eq!(matches("ab\\?c", "ac abc"), ["ac", "abc"]);
        assert_eq!(matches("a\\{2}", "aaaaa"), ["aa", "aa"]);
        assert_eq!(matches("a\\{2,}", "a aaa"), ["aaa"]);
        assert_eq!(matches("a\\{,2}b", "aaab"), ["aab"]);
        assert_eq!(matches("a\\{-1,}", "aaa"), ["a", "a", "a"]);
        assert_eq!(matches("a\\{-}b", "aab"), ["aab"]);
    }

    #[test]
    fn caret_dollar_and_star_are_special_only_where_vim_says() {
        assert_eq!(matches("a^b", "a^b"), ["a^b"]);
        assert_eq!(matches("a$b", "a$b"), ["a$b"]);
        assert_eq!(matches("*a", "*a"), ["*a"]);
        assert_eq!(matches("^*", "*a"), ["*"]);
        assert_eq!(matches("^a", "ab\nab"), ["a", "a"]);
        assert_eq!(matches("b$", "ab\nab"), ["b", "b"]);
        assert_eq!(matches("x\\|^a", "ab xa"), ["a", "x"]);
        assert_eq!(matches("\\(^a\\)", "ab ba"), ["a"]);
        assert_eq!(matches("b$\\|x", "ab\nx"), ["b", "x"]);
        assert_eq!(matches("b$\\|x", "ab x"), ["x"]);
    }

    #[test]
    fn very_magic_and_very_nomagic() {
        assert_eq!(matches("\\v(\\a+)(\\d{2})", "ab12 cd"), ["ab12"]);
        assert_eq!(matches("\\va|b", "abc"), ["a", "b"]);
        assert_eq!(matches("\\v<cat>", "cat concat"), ["cat"]);
        assert_eq!(matches("\\Va.c", "abc a.c"), ["a.c"]);
        assert_eq!(matches("\\Va\\.c", "abc a.c"), ["abc", "a.c"]);
        assert_eq!(matches("\\Ma*b", "aab a*b"), ["a*b"]);
        assert_eq!(matches("\\Ma\\*b", "aab a*b"), ["aab", "b"]);
    }

    #[test]
    fn classes_and_brackets_never_take_a_line_break_unless_told() {
        assert_eq!(matches("[^x]*", "ab\ncd"), ["ab", "", "cd", ""]);
        assert_eq!(matches("\\S\\+", "ab\ncd"), ["ab", "cd"]);
        assert_eq!(matches("\\_S\\+", "ab\ncd"), ["ab\ncd"]);
        assert_eq!(matches("a\\_.b", "a\nb"), ["a\nb"]);
        assert_eq!(matches("a\\nb", "a\nb"), ["a\nb"]);
        assert_eq!(matches("\\_[ab]\\+", "ab\nba"), ["ab\nba"]);
        assert_eq!(matches("[]\\-\\\\]", "a]b-c\\d"), ["]", "-", "\\"]);
        assert_eq!(matches("[a-c[:digit:]]\\+", "ab9z"), ["ab9"]);
        assert_eq!(matches("[[]", "a[b"), ["["]);
        assert_eq!(matches("a[b", "a[b"), ["a[b"]);
        assert_eq!(matches("\\d\\+", "ab12"), ["12"]);
        assert_eq!(matches("\\w\\+", "é ab_1"), ["ab_1"]);
        assert_eq!(matches("\\k\\+", "é ab_1"), ["é", "ab_1"]);
        assert_eq!(matches("\\u\\l", "Ab aB"), ["Ab"]);
        assert_eq!(matches("\\%x41\\%u00e9", "Aé"), ["Aé"]);
    }

    #[test]
    fn tilde_is_the_last_replacement_and_zs_ze_narrow_the_match() {
        assert_eq!(matches("x~", "xTILDE"), ["xTILDE"]);
        assert_eq!(matches("\\~", "a~"), ["~"]);
        assert_eq!(matches("foo\\zsbar", "foobar bar"), ["bar"]);
        assert_eq!(matches("foo\\zebar", "foobar foo"), ["foo"]);
        let pattern = Pattern::new("\\(a\\)\\zs\\(b\\)", Matching::Exact, "", Words::NONE).unwrap();
        let mut locations = pattern.locations();
        pattern.captures_at("ab", 0, &mut locations).unwrap();
        assert_eq!(
            pattern.group(&locations, 0),
            Some(Found { start: 1, end: 2 })
        );
        assert_eq!(
            pattern.group(&locations, 1),
            Some(Found { start: 0, end: 1 })
        );
        assert_eq!(
            pattern.group(&locations, 2),
            Some(Found { start: 1, end: 2 })
        );
    }

    #[test]
    fn case_is_matched_as_the_options_and_the_pattern_say() {
        let case = Case::default();
        let found = |source: &str, matching| {
            let pattern = Pattern::new(source, matching, "", Words::NONE).unwrap();
            pattern.find_at("Foo foo", 0).map(|found| found.start)
        };
        // Smart: a capital matches case, but not one after a backslash.
        assert_eq!(found("foo", Matching::Typed(case)), Some(0));
        assert_eq!(found("Foo", Matching::Typed(case)), Some(0));
        assert_eq!(found("fOo", Matching::Typed(case)), None);
        assert_eq!(found("\\Sfoo", Matching::Typed(case)), None);
        assert_eq!(found("\\Soo", Matching::Typed(case)), Some(0));
        assert_eq!(found("FOO\\c", Matching::Typed(case)), Some(0));
        assert_eq!(found("\\Cfoo", Matching::Typed(case)), Some(4));
        assert_eq!(found("foo", Matching::Exact), Some(4));
        assert_eq!(found("FOO", Matching::Ignore), Some(0));
        assert_eq!(found("FOo", Matching::Always(case)), Some(0));
    }

    #[test]
    fn capitals_and_small_letters_match_case_whatever_ignorecase_says() {
        let found = |source: &str, text: &str| {
            let pattern = Pattern::new(source, Matching::Ignore, "", Words::NONE).unwrap();
            let mut found = Vec::new();
            let mut at = 0;
            while let Some(one) = pattern.find_at(text, at) {
                found.push(text[one.start..one.end].to_owned());
                at = one.end.max(one.start + 1);
            }
            found
        };
        // As Neovim: `\u` and `\l` keep to case, a typed `[A-Z]` doesn't.
        assert_eq!(found("\\u", "aBc Def"), ["B", "D"]);
        assert_eq!(found("\\l\\+", "aBc"), ["a", "c"]);
        assert_eq!(found("\\U", "aB"), ["a"]);
        assert_eq!(found("[A-Z]", "aB"), ["a", "B"]);
        assert_eq!(found("\\_u", "a\nB"), ["\n", "B"]);
    }

    #[test]
    fn ze_alone_and_the_rest_of_the_classes_and_escapes() {
        // `\ze` without `\zs`: the match is up to it, and `\1` its group.
        let pattern = Pattern::new("\\(a\\)b\\zec", Matching::Exact, "", Words::NONE).unwrap();
        let mut locations = pattern.locations();
        let found = pattern.captures_at("abc", 0, &mut locations).unwrap();
        assert_eq!(found, Found { start: 0, end: 2 });
        assert_eq!(
            pattern.group(&locations, 1),
            Some(Found { start: 0, end: 1 })
        );
        // `\_K` and its kin take a line break too.
        assert_eq!(matches("x\\_Ky", "x\ny"), ["x\ny"]);
        assert_eq!(matches("x\\_I\\_F\\_p\\_Py", "x\n\n\n\ny"), ["x\n\n\n\ny"]);
        // `~*` repeats all of the last replacement.
        assert_eq!(matches("x~*", "xTILDETILDE"), ["xTILDETILDE"]);
        // Numbered characters in a class.
        assert_eq!(matches("[\\x41\\d66\\u0043]\\+", "ABCD"), ["ABC"]);
        assert_eq!(matches("[\\x]", "x\\"), ["x", "\\"]);
        // The file's start and end can't be found a line at a time.
        assert!(
            Pattern::new("\\%$", Matching::Exact, "", Words::NONE)
                .unwrap()
                .multiline()
        );
        // A huge pattern is refused, rather than compiled.
        let huge = "a\\{1000}".repeat(100);
        assert!(Pattern::new(&huge, Matching::Exact, "", Words::NONE).is_err());
    }

    #[test]
    fn word_edges_and_keywords_are_as_the_languages_words_say() {
        let words = Words::new("-");
        // `\<` and `\>` are at the edges of words with `-` in them, which
        // can start and end with it.
        assert_eq!(
            matches_in("\\<a-b\\>", "a-b x-a-b a-b-c (a-b)", words),
            ["a-b", "a-b"]
        );
        assert_eq!(matches_in("\\<--x\\>", "--x a--x --x-", words), ["--x"]);
        assert_eq!(matches_in("\\v<a>", "a a-b", words), ["a"]);
        // `\k` takes them, and `\K` too, but not digits.
        assert_eq!(
            matches_in("\\<\\k\\+\\>", "margin-top: 0", words),
            ["margin-top", "0"]
        );
        assert_eq!(matches_in("\\K\\+", "a1-b", words), ["a", "-b"]);
        // The groups, and what `\zs` narrows it to, are as they'd be
        // without them.
        assert_eq!(matches_in("\\<\\(a\\)-\\zsb\\>", "a-b c-b", words), ["b"]);
        let pattern = Pattern::new("\\<\\(a\\)-\\(b\\)", Matching::Exact, "", words).unwrap();
        let mut locations = pattern.locations();
        let found = pattern.captures_at("a-b", 0, &mut locations).unwrap();
        assert_eq!((found.start, found.end), (0, 3));
        let second = pattern.group(&locations, 2).unwrap();
        assert_eq!((second.start, second.end), (2, 3));
        // An alternative that doesn't match says nothing of the edges.
        assert_eq!(matches_in("x\\|\\<a\\>", "x-a a", words), ["x", "a"]);
        // The engine backtracks to find where the edges hold, as Vim does,
        // rather than taking the first match it would without them.
        assert_eq!(matches_in("\\<.*\\>", "color: red;", words), ["color: red"]);
        assert_eq!(
            matches_in("\\<.\\{-}\\>", "color: red;", words),
            ["color", "red"]
        );
        assert_eq!(matches_in("\\<\\k\\{-1,}\\>", "ab-cd", words), ["ab-cd"]);
        assert_eq!(
            matches_in("\\(foo\\|foo-bar\\)\\>", "foo-bar", words),
            ["foo-bar"]
        );
        assert_eq!(
            matches_in("\\<margin\\>\\|\\<margin-top\\>", "margin-top", words),
            ["margin-top"]
        );
        // In plain text, as Vim's: letters and digits, past ASCII as its
        // classes sort them, so `ª` and `²` are punctuation and a word of
        // CJK ideographs or of emoji is a word apart from letters.
        assert_eq!(matches("\\<a\\>", "a-b"), ["a"]);
        assert_eq!(matches("\\<a\\>", "ªa ²a éa"), ["a", "a"]);
        assert_eq!(matches("\\<b", "aªb a\u{a0}b é b"), ["b", "b", "b"]);
        assert_eq!(matches("\\<\\k\\+\\>", "x中y 😀z"), ["x中y", "😀z"]);
        assert_eq!(matches("\\<中\\>\\|\\<z", "x中y 😀z"), ["中", "z"]);
    }

    #[test]
    fn looking_around_matches_as_the_engines_own_edges_do() {
        // With words that have something in them the texts haven't, the
        // patterns are fancy-regex's, and find what the regex crate does.
        let words = Words::new("~");
        let text = "fn a_b(x: u8) -> u8 {\n  x * 2 // é and ÿ\n}\n\nlet αβ = a_b(1);\n";
        for source in [
            "\\<a_b\\>",
            "\\<\\w\\+\\>",
            "\\<\\k\\+\\>",
            "\\v<\\w+>",
            "\\<x\\>.*$",
            "^\\s*\\<\\a",
            "\\<\\(fn\\|let\\)\\>\\s\\+\\zs\\k\\+",
            "\\<\\u\\|\\<\\l\\+\\>",
            "\\c\\<FN\\>",
            "\\<[^ ]\\+\\>",
            "\\<.\\{-1,}\\>",
            "\\>\\_s\\+\\<",
            "\\<é\\>\\|\\<αβ\\>",
        ] {
            let pattern = Pattern::new(source, Matching::Exact, "", words).unwrap();
            assert!(matches!(pattern.engine, Engine::Looking { .. }), "{source}");
            assert_eq!(
                matches_in(source, text, words),
                matches(source, text),
                "{source}"
            );
        }
        // Without `\<` or `\>`, it's the regex crate's, words or none.
        let pattern = Pattern::new("\\k\\+", Matching::Exact, "", words).unwrap();
        assert!(matches!(pattern.engine, Engine::Plain(_)));
    }

    #[test]
    fn a_long_file_is_searched_a_line_at_a_time_without_giving_up() {
        // 10 MB of lines, each with a `color` in a word, and an `o` in
        // many, but no match: the steps back are counted a line at a time.
        let text = "a{background-color:red}\n".repeat(420_000);
        let words = Words::new("-");
        for source in ["\\<color\\>", "o.*z\\>"] {
            let pattern = Pattern::new(source, Matching::Exact, "", words).unwrap();
            assert_eq!(pattern.find_at(&text, 0), None, "{source}");
            assert!(!pattern.gave_up(), "{source}");
        }
        // And a match on the last line is found, and where it is.
        let text = text + "color\n";
        let pattern = Pattern::new("\\<color\\>", Matching::Exact, "", words).unwrap();
        let at = text.len() - "color\n".len();
        assert_eq!(
            pattern.find_at(&text, 0),
            Some(Found {
                start: at,
                end: at + 5
            })
        );
    }

    #[test]
    fn a_search_that_backtracks_too_much_gives_up_and_says_so() {
        let words = Words::new("-");
        // A `b` for the regex crate to find, so fancy-regex looks, but
        // never at a word's end.
        let text = "a".repeat(40) + "b-";
        let pattern = Pattern::new("\\(a*\\)*b\\>", Matching::Exact, "", words).unwrap();
        let start = std::time::Instant::now();
        assert_eq!(pattern.find_at(&text, 0), None);
        assert!(pattern.gave_up());
        assert_eq!(pattern.not_found("x".into()), TOO_COMPLEX);
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        // One that found nothing having looked everywhere didn't give up.
        let pattern = Pattern::new("\\<b\\>", Matching::Exact, "", words).unwrap();
        assert_eq!(pattern.find_at(&text, 0), None);
        assert!(!pattern.gave_up());
        assert_eq!(pattern.not_found("none".into()), "none");
    }

    #[test]
    fn what_the_engine_cant_do_is_refused() {
        for source in [
            "\\(a\\)\\1",
            "a\\@=",
            "a\\&b",
            "\\%V",
            "\\(a\\zsb\\)",
            "a\\zsb\\|c",
            "\\(a",
        ] {
            assert!(
                Pattern::new(source, Matching::Exact, "", Words::NONE).is_err(),
                "{source}"
            );
        }
        assert_eq!(regex("\\v\\{"), r"\{");
        assert_eq!(escape("a.b/c*"), "a\\.b\\/c\\*");
    }
}
