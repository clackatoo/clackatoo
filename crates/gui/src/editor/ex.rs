//! The editor's own commands on the command line, Vim's Ex commands for
//! lines: `:s`, `:&`, `:&&` and `:~`; `:g` and `:v`; `:d`, `:y`, `:m`,
//! `:t` (or `:co`), `:normal`, and `:format` over a range.
//!
//! A range alone goes to its last line: `:12`, `:+3`, `:$`, `:'a`. That's a
//! jump, as is any command that leaves the cursor on another line.
//!
//! Each takes a range of lines before its name, as Vim's do: a line's
//! number, `.` for the cursor's, `$` for the last, `'a` for a mark's (`'<`
//! and `'>` for the last selection's), `/pattern/` for the next line that
//! matches and `?pattern?` for the one before, each with `+` and `-` and a
//! number after; two of them with `,` between, or `;` to count the second
//! from the first; or `%` for every line. Without one, a command acts on
//! the cursor's line, and `:g` and `:v` on every line.
//!
//! `:g` marks the lines that match, and then does its command on each one
//! still there, in turn (see [`Marked`]). Its edits are made in a batch
//! (see [`BufferCopy::begin_batch`]), so the whole of it is one undo step
//! and one edit to the daemon, however many lines it changes. So is
//! `:normal` over a range, which types its keys on each line, as Vim's
//! does.

use crate::shortcuts::bindings::Hint;

use clackatoo_text::Edit;

use super::marked::Marked;
use super::pattern::{Found, Locations, Matching, Pattern, TOO_COMPLEX};
use super::substitute::{self, Bytes, Flags, Replacement};
use super::{BufferCopy, Done, Editor, Prompt, Register, Searched, Shared, Text, Wrote};
use crate::keymap::read_macro;
use crate::motion::{Cursor, Motion, Point};
use crate::vim::Mode;
use crate::vim::Recase;

/// The deepest macros typed by `:normal` type each other, as Vim's
/// `maxmapdepth`.
const DEEPEST_NORMAL: usize = 1000;

/// The most keys `:normal` types on a line, macros and all, before it's
/// stopped: a macro that types itself would go on for ever.
const MOST_NORMAL_KEYS: usize = 100_000;

/// More substitutions than this are told of in the bar, as Vim's `report`
/// says.
const REPORT: usize = 2;

/// The last `:s`, as `&`, `:&&` and `g&` make it again.
#[derive(Clone, Debug, Default)]
pub(crate) struct Substituted {
    pub(super) pattern: String,
    /// The replacement, its `~`s expanded, which a `~` in the next stands
    /// for.
    pub(super) replacement: String,
    pub(super) flags: Flags,
}

/// A command of the editor's, by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Name {
    Substitute,
    /// `:&`: the last `:s` again.
    Again,
    /// `:~`: the last `:s` again, with the last search's pattern.
    AgainSearched,
    Global,
    /// `:v`, and `:g!`: `:g` on the lines that don't match.
    Vglobal,
    Delete,
    Yank,
    Move,
    Copy,
    Normal,
    /// `:format` over a range: its lines formatted, as its language server
    /// does.
    Format,
}

/// The commands, by name, with the fewest letters each can be cut to.
const NAMES: [(&str, usize, Name); 10] = [
    ("substitute", 1, Name::Substitute),
    ("global", 1, Name::Global),
    ("vglobal", 1, Name::Vglobal),
    ("delete", 1, Name::Delete),
    ("yank", 1, Name::Yank),
    ("move", 1, Name::Move),
    ("copy", 2, Name::Copy),
    ("t", 1, Name::Copy),
    ("normal", 4, Name::Normal),
    ("format", 6, Name::Format),
];

/// The command `word`, all letters, names, if it names one.
fn named(word: &str) -> Option<Name> {
    (NAMES.iter())
        .find(|(name, least, _)| {
            word.len() >= *least
                && name
                    .get(..word.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(word))
        })
        .map(|&(_, _, name)| name)
}

/// Whether `line`, typed after `:`, is one of an editor's commands: it
/// starts with a range, or names one of them. A number alone is a tab, and
/// `-` alone, or before a kind of pane, a split.
pub(crate) fn is_editor_command(line: &str) -> bool {
    let line = line.trim_start();
    let mut chars = line.chars();
    match chars.next() {
        Some('%' | '.' | '$' | '\'' | '/' | '?' | ',' | ';' | '+' | '&' | '~' | '\\') => true,
        Some('0'..='9') => !(line.bytes()).all(|byte| byte.is_ascii_digit() || byte == b' '),
        Some('-') => chars
            .next()
            .is_some_and(|ch| ch.is_ascii_digit() || ch == ',' || ch == ';'),
        Some(ch) if ch.is_ascii_alphabetic() => {
            let word: String = line.chars().take_while(char::is_ascii_alphabetic).collect();
            named(&word).is_some()
        }
        _ => false,
    }
}

/// A match `:s///c` asks about, in the text as it is now.
#[derive(Clone, Debug)]
struct Asked {
    start: usize,
    /// The text matched, which must still be there to be replaced.
    matched: String,
    with: String,
}

/// `:s` with the `c` flag, asking in the bar about each match in turn.
#[derive(Debug)]
pub(crate) struct Confirming {
    /// The matches not yet asked about, the next first.
    asked: std::collections::VecDeque<Asked>,
    /// The replacement as typed, for the bar.
    shown: String,
    /// How many have been replaced, on how many lines, the start of the
    /// last one's line, and where the last replacement ended.
    count: usize,
    lines: usize,
    last_line: Option<usize>,
    last_end: Option<usize>,
}

impl Confirming {
    /// What the bar asks.
    pub(crate) fn question(&self) -> String {
        format!("replace with {} (y/n/a/q/l/^E/^Y)?", self.shown)
    }

    /// Moves the matches through `edit`, made to the text, by anyone.
    pub(crate) fn edited(&mut self, edit: &Edit) {
        for asked in &mut self.asked {
            asked.start = edit.map(asked.start, clackatoo_text::Bias::After);
        }
        if let Some(end) = &mut self.last_end {
            *end = edit.map(*end, clackatoo_text::Bias::Before);
        }
        if let Some(line) = &mut self.last_line {
            *line = edit.map(*line, clackatoo_text::Bias::Before);
        }
    }
}

/// What a line's range gave, as lines counted from 0, and whether one was
/// typed.
#[derive(Clone, Copy, Debug)]
pub(super) struct Lines {
    pub(super) first: u64,
    pub(super) last: u64,
    pub(super) typed: bool,
}

impl Editor {
    /// Does `line`, one of the editor's commands typed after `:`, as a
    /// macro's if `replaying`. `Err` says why it couldn't be done, which
    /// stops a macro typing it, as in Vim.
    pub(crate) fn ex(
        &mut self,
        line: &str,
        copy: &mut BufferCopy,
        shared: &mut Shared,
        replaying: bool,
    ) -> Result<Done, String> {
        if !copy.loaded() {
            return Err("the file hasn't come yet".into());
        }
        // The view goes back to where it was before the preview moved it,
        // and on from there.
        self.end_preview();
        self.leave_visual();
        self.prompt = None;
        self.status = None;
        self.replaying = replaying;
        self.new_step = true;
        // What the undo step it starts keeps, as for a key's command.
        self.step_cursor = None;
        self.step_visual = shared.marks.step_visual(copy.file(), copy.text());
        self.saved = None;
        self.edits_before = copy.edits_counted();
        let from = self.cursor.at;
        let done = self.run(line, copy, shared);
        if self.mode != Mode::Insert {
            self.end_saved(copy);
        }
        self.replaying = false;
        if self.mode != Mode::Insert {
            self.normal_cursor(copy);
        }
        if self.cursor.at.line != from.line {
            self.left_from(from, copy);
        }
        self.follow(copy);
        done.map(|()| Done::REDRAW)
    }

    /// Does `line`, read from its range on.
    pub(super) fn run(
        &mut self,
        line: &str,
        copy: &mut BufferCopy,
        shared: &mut Shared,
    ) -> Result<(), String> {
        let mut rest = line.trim_start_matches([' ', ':']);
        let lines = self.range(&mut rest, copy, shared)?;
        rest = rest.trim_start();
        // A range alone goes to its last line, as `:12`, `:+3` and `:$` do,
        // keeping the column as `G` does, or with `startofline` to its
        // first non-blank.
        if rest.is_empty()
            && let Some(lines) = lines
        {
            let text = self.text(copy);
            let mut cursor = self.cursor;
            cursor.onto_line(lines.last, self.startofline, &text);
            drop(text);
            self.cursor = cursor;
            return Ok(());
        }
        let (name, bang, after) = command_name(rest)?;
        let current = self.cursor.at.line;
        let default = Lines {
            first: current,
            last: current,
            typed: false,
        };
        let lines = lines.unwrap_or(default);
        match name {
            Name::Substitute | Name::Again | Name::AgainSearched => {
                let typed = if name == Name::Substitute {
                    substitute::read(after)?
                } else {
                    // `:&&`'s second `&` is the flag that keeps the last
                    // flags.
                    let mut typed = substitute::read(&format!(" {after}"))?;
                    typed.pattern = None;
                    typed
                };
                self.substitute(lines, &typed, name == Name::AgainSearched, copy, shared)
            }
            Name::Global | Name::Vglobal => {
                if self.in_global {
                    return Err(":g can't be done by a :g".into());
                }
                let lines = if lines.typed {
                    lines
                } else {
                    let last = self.text(copy).last_line();
                    Lines {
                        first: 0,
                        last,
                        typed: true,
                    }
                };
                self.global(lines, name == Name::Vglobal || bang, after, copy, shared)
            }
            Name::Delete | Name::Yank => {
                let (register, lines) = self.register_and_count(after, lines, copy)?;
                if name == Name::Delete {
                    // As Vim's `ex_operators()` puts the cursor before it
                    // changes anything, for the undo step to keep.
                    let text = self.text(copy);
                    let mut cursor = self.cursor;
                    cursor.onto_line(lines.first, self.startofline, &text);
                    drop(text);
                    self.step_cursor = Some(cursor.at);
                    self.delete_lines(lines, register, copy, shared);
                } else {
                    self.yank_lines(lines, register, copy, shared);
                }
                Ok(())
            }
            Name::Move | Name::Copy => {
                let mut after = after.trim();
                let Some(to) = self.address(&mut after, copy, shared)? else {
                    return Err(format!(
                        "say where to {}, as :{} 0 for the top",
                        if name == Name::Move {
                            "move them"
                        } else {
                            "copy them"
                        },
                        if name == Name::Move { "m" } else { "t" }
                    ));
                };
                if !after.trim().is_empty() {
                    return Err(format!("{} isn't a line", after.trim()));
                }
                if name == Name::Move {
                    self.move_lines(lines, to, copy)
                } else {
                    self.copy_lines(lines, to, copy);
                    Ok(())
                }
            }
            Name::Format => self.format_lines(lines, after, copy),
            Name::Normal => {
                let keys = after.strip_prefix(' ').unwrap_or(after);
                if keys.is_empty() {
                    return Err(":normal needs keys to type, as :normal Atext".into());
                }
                if self.in_global {
                    return self.normal(keys, copy, shared);
                }
                let starts = self.line_starts(lines, copy);
                self.each_line(starts, copy, shared, &mut |editor, copy, shared| {
                    editor.normal(keys, copy, shared)
                })
            }
        }
    }

    /// `:format` over `lines`, with `after` typed after it: from the first
    /// line's start to the last one's end, not into the line after, for
    /// the desk to have the language server format.
    fn format_lines(&mut self, lines: Lines, after: &str, copy: &BufferCopy) -> Result<(), String> {
        if self.in_global {
            return Err(":format can't be done by a :g".into());
        }
        if !after.trim().is_empty() {
            return Err(format!(
                ":format takes nothing after it, but has {:?}",
                after.trim()
            ));
        }
        let text = self.text(copy);
        let range = (text.line_start(lines.first), text.line_end(lines.last));
        drop(text);
        self.format_asked = Some(range);
        Ok(())
    }

    // --- Ranges -------------------------------------------------------------

    /// The range `text` starts with, which is read past, if it has one.
    pub(super) fn range(
        &mut self,
        text: &mut &str,
        copy: &BufferCopy,
        shared: &Shared,
    ) -> Result<Option<Lines>, String> {
        let count = self.text(copy).lines();
        // All of it, its wanted column too, to put back.
        let cursor = self.cursor;
        let mut lines: Vec<u64> = Vec::new();
        let result = loop {
            *text = text.trim_start();
            if let Some(rest) = text.strip_prefix('%') {
                *text = rest;
                lines.extend([1, count]);
            } else {
                match self.address(text, copy, shared) {
                    Ok(Some(line)) => lines.push(line),
                    Ok(None) => {}
                    Err(why) => break Err(why),
                }
            }
            *text = text.trim_start();
            let Some(separator @ (',' | ';')) = text.chars().next() else {
                break Ok(());
            };
            *text = &text[1..];
            let current = self.cursor.at.line + 1;
            if lines.is_empty() {
                lines.push(current);
            }
            let next = text.trim_start();
            if next.is_empty() || !starts_address(next) {
                // `:5,` is `:5,.`.
                lines.push(current);
            }
            if separator == ';' {
                // The next is counted from this one.
                let line = *lines.last().expect("one was just pushed");
                self.cursor = Cursor::new(Point {
                    line: line.saturating_sub(1).min(count - 1),
                    col: 0,
                });
            }
        };
        self.cursor = cursor;
        result?;
        let Some(&last) = lines.last() else {
            return Ok(None);
        };
        let first = lines
            .len()
            .checked_sub(2)
            .map_or(last, |index| lines[index]);
        // A line alone is gone to, as far as the file goes, as in Vim.
        let alone = text.trim().is_empty();
        let (first, last) = if alone {
            (first.min(count), last.min(count))
        } else {
            (first, last)
        };
        for line in [first, last] {
            if line > count {
                return Err(match count {
                    1 => format!("there's no line {line}; the file has one"),
                    _ => format!("there's no line {line}; the file has {count}"),
                });
            }
        }
        // Line 0 is the first, as in Vim, and a range backwards is turned
        // round.
        let (first, last) = (first.min(last), first.max(last));
        Ok(Some(Lines {
            first: first.saturating_sub(1),
            last: last.saturating_sub(1),
            typed: true,
        }))
    }

    /// The line the address `text` starts with is, counting from 1, with 0
    /// before the first, read past, if it starts with one.
    fn address(
        &mut self,
        text: &mut &str,
        copy: &BufferCopy,
        shared: &Shared,
    ) -> Result<Option<u64>, String> {
        let count = self.text(copy).lines().cast_signed();
        let current = self.cursor.at.line.cast_signed() + 1;
        let mut chars = text.chars();
        let mut line = match chars.next() {
            Some('0'..='9') => {
                let digits = text.bytes().take_while(u8::is_ascii_digit).count();
                let number = text[..digits]
                    .parse::<i64>()
                    .map_err(|_| format!("{} isn't a line", &text[..digits]))?;
                *text = &text[digits..];
                number
            }
            Some('.') => {
                *text = &text[1..];
                current
            }
            Some('$') => {
                *text = &text[1..];
                count
            }
            Some('\'') => {
                let Some(mark) = chars.next() else {
                    return Err("' needs a mark after it, as 'a".into());
                };
                *text = &text[1 + mark.len_utf8()..];
                let mark = if mark == '`' { '\'' } else { mark };
                let place = (shared.marks.get(mark, copy.file()))
                    .filter(|place| place.is_in(copy.file()))
                    .ok_or_else(|| format!("mark {mark} isn't set in this file"))?;
                place.line.cast_signed() + 1
            }
            Some(delimiter @ ('/' | '?')) => {
                let (pattern, rest) = until(&text[1..], delimiter);
                *text = rest;
                let pattern = self.pattern_or_last(&pattern)?;
                self.line_matching(&pattern, delimiter == '?', copy, shared)? + 1
            }
            Some('\\') => {
                let (backward, pattern) = match chars.next() {
                    Some('/') => (
                        false,
                        self.search.as_ref().map(|search| search.pattern.clone()),
                    ),
                    Some('?') => (
                        true,
                        self.search.as_ref().map(|search| search.pattern.clone()),
                    ),
                    Some('&') => (
                        false,
                        shared.substituted.as_ref().map(|last| last.pattern.clone()),
                    ),
                    _ => return Err("\\ starts no address but \\/, \\? and \\&".into()),
                };
                *text = &text[2..];
                let pattern = pattern.ok_or("there's no pattern to use yet")?;
                self.line_matching(&pattern, backward, copy, shared)? + 1
            }
            Some('+' | '-') => current,
            _ => return Ok(None),
        };
        // Offsets: `+3`, `-`, and a number alone after another, which adds.
        while let Some(sign) = text.chars().next() {
            let by = match sign {
                '+' | '-' => 1,
                '0'..='9' => 0,
                _ => break,
            };
            let rest = if by == 1 { &text[1..] } else { *text };
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            let number = if digits == 0 {
                1
            } else {
                rest[..digits]
                    .parse::<i64>()
                    .map_err(|_| "a number too big".to_owned())?
            };
            *text = &rest[digits..];
            line = if sign == '-' {
                line - number
            } else {
                line.saturating_add(number)
            };
        }
        if line < 0 {
            return Err(format!("line {line} is before the file's start"));
        }
        Ok(Some(line.cast_unsigned()))
    }

    /// `source`, a pattern, or the last search's if it's empty.
    pub(super) fn pattern_or_last(&self, source: &str) -> Result<String, String> {
        if !source.is_empty() {
            return Ok(source.to_owned());
        }
        (self.search.as_ref())
            .map(|search| search.pattern.clone())
            .ok_or_else(|| "there's no search to use yet".into())
    }

    /// The line, from 0, of the next match of `source` after the cursor's
    /// line, or before it if `backward`, going round the file's ends. It's
    /// the last search's from then on, as in Vim.
    fn line_matching(
        &mut self,
        source: &str,
        backward: bool,
        copy: &BufferCopy,
        shared: &Shared,
    ) -> Result<i64, String> {
        let pattern = self.compile(source, None, copy, shared)?;
        self.remember_search(source);
        let (whole, len) = self.haystack(copy);
        let hay = &whole[..len];
        let text = self.text(copy);
        let line = self.cursor.at.line;
        let starts_line = |byte: usize| text.rope().byte_to_line(byte) as u64;
        let found = if backward {
            // The last before the cursor's line, or else the file's last.
            let before = text.line_start(line);
            let (mut last_before, mut last) = (None, None);
            let mut at = 0;
            while at <= hay.len()
                && let Some(found) = pattern.find_at(hay, at)
            {
                if found.start < before {
                    last_before = Some(found.start);
                }
                last = Some(found.start);
                at = next_from(hay, found);
            }
            last_before.or(last)
        } else {
            let after = text.line_start(line + 1);
            (pattern.find_at(hay, after.min(hay.len())))
                .filter(|found| found.start >= after)
                .or_else(|| pattern.find_at(hay, 0))
                .map(|found| found.start)
        };
        let found =
            found.ok_or_else(|| pattern.not_found(format!("{source} isn't in the file")))?;
        Ok(starts_line(found).cast_signed())
    }

    /// The register and count after `:d` or `:y`, and the lines they act
    /// on with the count: from the range's last, that many.
    fn register_and_count(
        &self,
        text: &str,
        lines: Lines,
        copy: &BufferCopy,
    ) -> Result<(Option<char>, Lines), String> {
        let mut text = text.trim();
        let mut register = None;
        if let Some(ch) = text.chars().next().filter(|ch| !ch.is_ascii_digit()) {
            if !crate::vim::is_register(ch) {
                return Err(format!("{ch} isn't a register"));
            }
            register = Some(ch);
            text = text[ch.len_utf8()..].trim_start();
        }
        Ok((register, self.counted(text, lines, copy)?))
    }

    /// `lines`, or with a count in `text`, that many from their last.
    fn counted(&self, text: &str, lines: Lines, copy: &BufferCopy) -> Result<Lines, String> {
        if text.is_empty() {
            return Ok(lines);
        }
        let count: u64 = (text.parse().ok())
            .filter(|&count| count > 0)
            .ok_or_else(|| format!("{text} isn't a count of lines"))?;
        let last = self.text(copy).last_line();
        Ok(Lines {
            first: lines.last,
            last: (lines.last + count - 1).min(last),
            typed: true,
        })
    }

    // --- :s -----------------------------------------------------------------

    /// Compiles `source`, with case matched as `exact` says if it says, or
    /// as the options do; `~` in it is the last replacement.
    pub(super) fn compile(
        &self,
        source: &str,
        exact: Option<bool>,
        copy: &BufferCopy,
        shared: &Shared,
    ) -> Result<Pattern, String> {
        let matching = match exact {
            Some(true) => Matching::Exact,
            Some(false) => Matching::Ignore,
            None => Matching::Typed(self.case),
        };
        self.pattern(source, matching, copy, shared)
    }

    /// `source` compiled, matching case as `matching` says, with `~` the
    /// last replacement, and words as `copy`'s language reads them. The
    /// last compiled is kept, so a `:g` compiles its command's pattern
    /// once, not on every line, and a preview once, not at every key.
    pub(super) fn pattern(
        &self,
        source: &str,
        matching: Matching,
        copy: &BufferCopy,
        shared: &Shared,
    ) -> Result<Pattern, String> {
        let words = copy.words();
        let tilde = shared
            .substituted
            .as_ref()
            .map_or("", |last| last.replacement.as_str());
        let mut kept = self.compiled.borrow_mut();
        if let Some(kept) = kept.as_ref()
            && kept.source == source
            && kept.matching == matching
            && kept.tilde == tilde
            && kept.words == words
        {
            kept.pattern.searching(&self.haystack(copy).0);
            return Ok(kept.pattern.clone());
        }
        let pattern = Pattern::new(source, matching, tilde, words)?;
        pattern.searching(&self.haystack(copy).0);
        *kept = Some(super::Compiled {
            source: source.to_owned(),
            matching,
            tilde: tilde.to_owned(),
            words,
            pattern: pattern.clone(),
        });
        Ok(pattern)
    }

    /// The copy's whole text, as one string, and how much of it is
    /// matched: all but the line break that ends the last line, if it
    /// ends with one, which is never. It's kept until the text changes, so
    /// `n` and a preview's keys don't each copy a long file.
    pub(super) fn haystack(&self, copy: &BufferCopy) -> (std::rc::Rc<str>, usize) {
        let of = (copy.buffer(), copy.revision());
        let mut kept = self.whole.borrow_mut();
        let whole = match kept.as_ref() {
            Some(kept) if kept.of == of => std::rc::Rc::clone(&kept.text),
            _ => {
                let text: std::rc::Rc<str> = copy.text().to_string().into();
                *kept = Some(super::Whole {
                    of,
                    text: std::rc::Rc::clone(&text),
                });
                text
            }
        };
        let len = whole.strip_suffix('\n').map_or(whole.len(), str::len);
        (whole, len)
    }

    /// Makes `source` the last search, for `n` and `:s//`, going the way
    /// the last went.
    fn remember_search(&mut self, source: &str) {
        let backward = self.search.as_ref().is_some_and(|search| search.backward);
        self.search = Some(Searched {
            pattern: source.to_owned(),
            backward,
            star: false,
        });
    }

    /// `:s`, as `typed`, on `lines`; with the last search's pattern if
    /// `searched`, as `:~` does.
    fn substitute(
        &mut self,
        lines: Lines,
        typed: &substitute::Typed,
        searched: bool,
        copy: &mut BufferCopy,
        shared: &mut Shared,
    ) -> Result<(), String> {
        let last = shared.substituted.clone();
        let last_flags = last.as_ref().map(|last| last.flags).unwrap_or_default();
        let flags = Flags::read(&typed.flags, last_flags)?;
        let lines = self.counted(
            &typed
                .count
                .map(|count| count.to_string())
                .unwrap_or_default(),
            lines,
            copy,
        )?;
        let searched = searched || flags.last_search;
        let (pattern, replacement) = self.substitution(typed, searched, last.as_ref())?;
        let compiled = self.compile(&pattern, flags.exact, copy, shared)?;
        let with = Replacement::new(&replacement)?;
        shared.substituted = Some(Substituted {
            pattern: pattern.clone(),
            replacement: replacement.clone(),
            flags,
        });
        self.remember_search(&pattern);
        let text = self.text(copy);
        let total = text.rope().len_bytes();
        let (start, end) = (text.line_start(lines.first), text.line_end(lines.last));
        // A pattern that can't cross lines is searched for in the lines
        // alone, so `:g`, which asks a line at a time, doesn't copy the
        // whole text for each.
        let (hay, base): (std::rc::Rc<str>, usize) = if compiled.multiline() {
            drop(text);
            let (whole, len) = self.haystack(copy);
            (whole[..len].into(), 0)
        } else {
            (text.slice(start, end).into(), start)
        };
        let hay = &*hay;
        let bytes = Bytes {
            start: start - base,
            end: (end - base).min(hay.len()),
        };
        let not_found = || compiled.not_found(format!("{pattern} isn't in the lines given"));
        // Nothing found, or only some of what's there, as when a search
        // gave up, which nothing is done to: said, unless `quiet` and it
        // found nothing.
        let none = |quiet: bool| {
            if quiet && !compiled.gave_up() {
                Ok(())
            } else {
                Err(not_found())
            }
        };
        if flags.count_only {
            let (count, on) = count_matches(hay, &compiled, bytes, flags.all);
            if count == 0 || compiled.gave_up() {
                return none(flags.quiet || self.in_global);
            }
            self.counted_matches.0 += count;
            self.counted_matches.1 += on;
            if !self.in_global {
                self.status = Some(substitute::report(count, on, true));
            }
            return Ok(());
        }
        if flags.confirm && self.in_global {
            return Err(":s with c can't be done by a :g yet".into());
        }
        if flags.confirm {
            return self
                .confirm(
                    (hay, base),
                    &compiled,
                    &with,
                    bytes,
                    flags.all,
                    &replacement,
                )
                .map_or_else(
                    || none(flags.quiet),
                    |()| {
                        self.show_asked(copy);
                        Ok(())
                    },
                );
        }
        let at = (base, total);
        let replaced = substitute::replace(hay, at, &compiled, &with, bytes, flags.all);
        if replaced.count == 0 || compiled.gave_up() {
            return none(flags.quiet || self.in_global);
        }
        // As Vim's `do_sub()` puts the cursor before its first change, for
        // the undo step to keep: a step though it changes nothing.
        if let Some((at, _, _)) = replaced.edit.changes().next() {
            let line = self.text(copy).point_of(at).line;
            self.step_cursor = Some(Point { line, col: 0 });
        }
        self.save_step(copy);
        self.make(replaced.edit, copy);
        if let Some(end) = replaced.last_end {
            self.cursor_to_line_of(end, copy);
        }
        self.counted_matches.0 += replaced.count;
        self.counted_matches.1 += replaced.lines;
        if !self.in_global && replaced.count > REPORT {
            self.status = Some(substitute::report(replaced.count, replaced.lines, false));
        }
        Ok(())
    }

    /// The pattern and the replacement, its `~`s expanded, of `:s` typed
    /// as `typed`, after `last`, the last; with no pattern, the last's, or
    /// if `searched`, the last search's.
    fn substitution(
        &self,
        typed: &substitute::Typed,
        searched: bool,
        last: Option<&Substituted>,
    ) -> Result<(String, String), String> {
        if let Some(pattern) = &typed.pattern {
            let pattern = self.pattern_or_last(pattern)?;
            let previous = last.map_or("", |last| last.replacement.as_str());
            let replacement = substitute::expand_tilde(&typed.replacement, previous);
            return Ok((pattern, replacement));
        }
        let last = last.ok_or("there's no :s to do again yet")?;
        let pattern = if searched {
            self.pattern_or_last("")?
        } else {
            last.pattern.clone()
        };
        Ok((pattern, last.replacement.clone()))
    }

    /// Puts the cursor on the first character that isn't blank of the line
    /// byte `byte` is on, as `:s` leaves it on its last line.
    fn cursor_to_line_of(&mut self, byte: usize, copy: &BufferCopy) {
        let text = self.text(copy);
        let line = text.point_of(byte).line.min(text.last_line());
        self.cursor = Cursor::new(Point { line, col: 0 });
        self.cursor.apply(Motion::FirstNonBlank, &text);
    }

    /// Starts asking about each match of `:s///c` in the bar, if there are
    /// any: `None` if not.
    fn confirm(
        &mut self,
        (hay, base): (&str, usize),
        pattern: &Pattern,
        with: &Replacement,
        bytes: Bytes,
        all: bool,
        shown: &str,
    ) -> Option<()> {
        let mut asked = std::collections::VecDeque::new();
        substitute::each_match(
            hay,
            pattern,
            bytes,
            (all, with.uses_groups()),
            &mut |found, locations: &Locations| {
                let mut replacement = String::new();
                with.expand(hay, pattern, (found, locations), &mut replacement);
                asked.push_back(Asked {
                    start: base + found.start,
                    matched: hay[found.start..found.end].to_owned(),
                    with: replacement,
                });
                true
            },
        );
        // Nothing's asked about where a search gave up, as only some of
        // the matches were found.
        if asked.is_empty() || pattern.gave_up() {
            return None;
        }
        self.prompt = Some(Prompt::Confirm(Confirming {
            asked,
            shown: shown.to_owned(),
            count: 0,
            lines: 0,
            last_line: None,
            last_end: None,
        }));
        Some(())
    }

    /// Shows the match `:s///c` asks about next, with the cursor on it, or
    /// finishes if there's none left.
    pub(super) fn show_asked(&mut self, copy: &BufferCopy) {
        let Some(Prompt::Confirm(confirming)) = &self.prompt else {
            return;
        };
        let text = self.text(copy);
        let next = confirming.asked.front().map(|asked| {
            let end = asked.start + asked.matched.len();
            (
                text.point_of(asked.start),
                text.point_of(end.saturating_sub(1).max(asked.start)),
            )
        });
        drop(text);
        match next {
            Some((start, last)) => {
                self.cursor = Cursor::new(start);
                self.found = Some((start, last));
                self.follow(copy);
            }
            None => self.finish_confirming(copy),
        }
    }

    // A key typed while `:s//c` asks about a match: `y` replaces it, `l`
    // replaces it and stops, `n` leaves it, `a` replaces it and all the
    // rest, `q` and Esc stop, and Ctrl+E and Ctrl+Y scroll.
    crate::shortcuts::bindings::described_keys! {
        pub(super) fn confirm_stroke / describe_confirm_stroke(&mut self, stroke: crate::vim::Stroke) -> ();
        execute(copy: &mut BufferCopy);
        prepare {
            use crate::vim::Stroke;
        }
        match stroke => {
            Stroke::Char {
                ch: 'y',
                ctrl: false,
            } =>
                Hint::Action("Replace this match"),
                self.replace_asked(1, copy);
            Stroke::Char {
                ch: 'l',
                ctrl: false,
            } =>
                Hint::Action("Replace this match and stop"),
                {
                    self.replace_asked(1, copy);
                    return self.finish_confirming(copy);
                };
            Stroke::Char {
                ch: 'a',
                ctrl: false,
            } =>
                Hint::Action("Replace all remaining matches"),
                self.replace_asked(usize::MAX, copy);
            Stroke::Char {
                ch: 'n',
                ctrl: false,
            } =>
                Hint::Action("Skip this match"),
                {
                    if let Some(Prompt::Confirm(confirming)) = &mut self.prompt {
                        confirming.asked.pop_front();
                    }
                };
            Stroke::Char {
                ch: 'q',
                ctrl: false,
            }
            | Stroke::Escape
            | Stroke::Char {
                ch: 'c',
                ctrl: true,
            } =>
                Hint::Action("Stop replacing"),
                return self.finish_confirming(copy);
            Stroke::Char {
                ch: 'e',
                ctrl: true,
            } =>
                Hint::Action("Scroll down"),
                return self.wheel(1, copy);
            Stroke::Char {
                ch: 'y',
                ctrl: true,
            } =>
                Hint::Action("Scroll up"),
                return self.wheel(-1, copy);
            _ =>
                Hint::None,
                return;
        }
        then self.show_asked(copy)
    }

    /// Replaces the next `times` matches `:s///c` asks about, as one edit,
    /// joining the undo step of those replaced before.
    fn replace_asked(&mut self, times: usize, copy: &mut BufferCopy) {
        let Some(Prompt::Confirm(confirming)) = &mut self.prompt else {
            return;
        };
        let text = copy.text();
        let len = text.len_bytes();
        let mut edit = Edit::new();
        let mut through = 0;
        let mut longer = 0isize;
        let (mut taken, mut made) = (0, false);
        while taken < times
            && let Some(asked) = confirming.asked.pop_front()
        {
            taken += 1;
            let end = asked.start + asked.matched.len();
            // Someone else may have changed it meanwhile.
            if end > len
                || asked.start < through
                || text
                    .get_byte_slice(asked.start..end)
                    .is_none_or(|was| was != asked.matched.as_str())
            {
                continue;
            }
            edit.retain(asked.start - through)
                .delete(&asked.matched)
                .insert(&asked.with);
            through = end;
            made = true;
            let start = asked.start.cast_signed() + longer;
            longer += asked.with.len().cast_signed() - asked.matched.len().cast_signed();
            let line_start = text.line_to_byte(text.byte_to_line(asked.start));
            if confirming.last_line != Some(line_start) {
                confirming.lines += 1;
            }
            confirming.last_line = Some(line_start);
            confirming.last_end = Some(start.cast_unsigned() + asked.with.len());
            confirming.count += 1;
        }
        if !made {
            return;
        }
        edit.retain(len - through);
        // The last line and end are in the text before this edit, but for
        // the end just worked out.
        let end = confirming.last_end;
        let line = confirming.last_line;
        for asked in &mut confirming.asked {
            asked.start = edit.map(asked.start, clackatoo_text::Bias::After);
        }
        confirming.last_line = line.map(|line| edit.map(line, clackatoo_text::Bias::Before));
        confirming.last_end = end;
        self.make(edit, copy);
    }

    /// Stops asking about `:s///c`'s matches, leaving the cursor on the
    /// last line one was replaced on.
    fn finish_confirming(&mut self, copy: &BufferCopy) {
        let Some(Prompt::Confirm(confirming)) = self.prompt.take() else {
            return;
        };
        self.found = None;
        if let Some(end) = confirming.last_end {
            self.cursor_to_line_of(end, copy);
        }
        if confirming.count > REPORT {
            self.status = Some(substitute::report(
                confirming.count,
                confirming.lines,
                false,
            ));
        }
        self.normal_cursor(copy);
        self.follow(copy);
    }

    // --- :g and :normal -----------------------------------------------------

    /// `:g/pattern/command`, or if `invert`, `:v`: does `command` on each
    /// line of `lines` that matches, or doesn't.
    fn global(
        &mut self,
        lines: Lines,
        invert: bool,
        text: &str,
        copy: &mut BufferCopy,
        shared: &mut Shared,
    ) -> Result<(), String> {
        let text_typed = text.trim_start();
        let Some(delimiter) = text_typed.chars().next() else {
            return Err(":g needs a pattern and a command, as :g/pattern/d".into());
        };
        if delimiter.is_ascii_alphanumeric() || matches!(delimiter, '\\' | '"' | '|') {
            return Err(format!("{delimiter} can't separate :g's pattern"));
        }
        let (pattern, command) = until(&text_typed[delimiter.len_utf8()..], delimiter);
        let pattern = self.pattern_or_last(&pattern)?;
        if command.trim().is_empty() {
            return Err(":g needs a command to do on each line, as :g/pattern/d".into());
        }
        let compiled = self.compile(&pattern, None, copy, shared)?;
        self.remember_search(&pattern);
        let starts = {
            let (whole, len) = self.haystack(copy);
            matching_lines(&whole[..len], &compiled, &self.text(copy), lines, invert)
        };
        // Nor are only some of the lines that match acted on.
        if compiled.gave_up() {
            return Err(TOO_COMPLEX.into());
        }
        if starts.is_empty() {
            return Err(if invert {
                format!("every line has {pattern}")
            } else {
                format!("{pattern} isn't in the lines given")
            });
        }
        self.counted_matches = (0, 0);
        self.in_global = true;
        let done = self.each_line(starts, copy, shared, &mut |editor, copy, shared| {
            editor.run(command, copy, shared)
        });
        self.in_global = false;
        let (count, on) = std::mem::take(&mut self.counted_matches);
        if done.is_ok() && count > REPORT {
            self.status = Some(substitute::report(count, on, false));
        }
        done
    }

    /// The starts of `lines`, each of them.
    fn line_starts(&self, lines: Lines, copy: &BufferCopy) -> Vec<usize> {
        let text = self.text(copy);
        (lines.first..=lines.last.min(text.last_line()))
            .map(|line| text.line_start(line))
            .collect()
    }

    /// Does `act` on each line starting at `starts` that's still there, in
    /// turn, with the cursor at its start, making every edit in one batch:
    /// one undo step, and one edit to the daemon.
    fn each_line(
        &mut self,
        starts: Vec<usize>,
        copy: &mut BufferCopy,
        shared: &mut Shared,
        act: &mut dyn FnMut(&mut Self, &mut BufferCopy, &mut Shared) -> Result<(), String>,
    ) -> Result<(), String> {
        // Worked out before anything's done: a macro's first edit to the
        // file starts its undo step.
        let step = if self.replaying {
            copy.first_macro_edit()
        } else {
            true
        };
        copy.begin_batch();
        let mut marked = Marked::new(starts);
        let mut done = Ok(());
        for index in 0..marked.len() {
            let Some(at) = marked.get(index) else {
                continue;
            };
            let line = copy.text().byte_to_line(at.min(copy.text().len_bytes())) as u64;
            self.cursor = Cursor::new(Point { line, col: 0 });
            let (made, mut before) = (copy.batched().len(), copy.text().clone());
            done = act(self, copy, shared);
            for edit in &copy.batched()[made..] {
                marked.edited(edit, &before);
                if edit.apply(&mut before).is_err() {
                    log::error!("an edit :g made didn't fit");
                }
            }
            if done.is_err() {
                break;
            }
        }
        copy.end_batch(step);
        done
    }

    /// `:normal`: types `keys` in normal mode, as if typed, finishing what
    /// they leave half done as Esc would.
    fn normal(
        &mut self,
        keys: &str,
        copy: &mut BufferCopy,
        shared: &mut Shared,
    ) -> Result<(), String> {
        if self.mode != Mode::Normal {
            self.stroke(crate::vim::Stroke::Escape, copy, shared);
        }
        let mut pending: Vec<std::vec::IntoIter<crate::keymap::Chord>> =
            vec![read_macro(keys).into_iter()];
        let mut typed = 0;
        while let Some(frame) = pending.last_mut() {
            let Some(chord) = frame.next() else {
                pending.pop();
                continue;
            };
            typed += 1;
            if typed > MOST_NORMAL_KEYS {
                return Err(format!(
                    ":normal typed {MOST_NORMAL_KEYS} keys on a line, so it stopped"
                ));
            }
            let kept = chord.kept();
            for stroke in self.strokes(&kept.press()) {
                self.failed = false;
                self.stroke(stroke, copy, shared);
                if self.awaiting.take().is_some() {
                    self.resume_replaying = false;
                    return Err(":normal can't type keys that wait for the grammar".into());
                }
                if let Some((keys, times)) = self.macro_asked.take() {
                    if pending.len() >= DEEPEST_NORMAL {
                        return Err("macros typed by :normal went too deep".into());
                    }
                    let keys: Vec<_> = (0..times).flat_map(|_| keys.iter().copied()).collect();
                    pending.push(keys.into_iter());
                }
                if self.failed {
                    // The rest of the keys are for what failed.
                    pending.clear();
                    break;
                }
            }
        }
        self.failed = false;
        // What's left half done is finished, or given up, as Esc does.
        if matches!(self.prompt, Some(Prompt::Search { .. })) {
            self.prompt = None;
        }
        self.keys.clear();
        if self.mode != Mode::Normal {
            self.stroke(crate::vim::Stroke::Escape, copy, shared);
        }
        Ok(())
    }

    // --- :d, :y, :m and :t --------------------------------------------------

    /// The text of `lines`, ending with a line break, where they start and
    /// end, and whether the last is the file's last, with no break after.
    fn lines_text(&self, lines: Lines, copy: &BufferCopy) -> (String, usize, usize, bool) {
        let text = self.text(copy);
        let start = text.line_start(lines.first);
        let end = text.line_start(lines.last + 1);
        let mut block = text.slice(start, end);
        let unended = !block.ends_with('\n');
        if unended {
            block.push('\n');
        }
        (block, start, end, unended)
    }

    /// `:d`: deletes `lines`, keeping them in `register`.
    fn delete_lines(
        &mut self,
        lines: Lines,
        register: Option<char>,
        copy: &mut BufferCopy,
        shared: &mut Shared,
    ) {
        let (block, ..) = self.lines_text(lines, copy);
        let history = Wrote::Delete {
            small: false,
            history: true,
        };
        shared
            .registers
            .wrote(register, Register::new(block, true), history);
        // As Vim's: only `dd` and its kin keep the column.
        let text = self.text(copy);
        let span = super::Span::lines(&text, lines.first, lines.last).leaving_first_non_blank();
        drop(text);
        self.delete_span(span, copy);
        self.report_lines(lines, "fewer lines");
    }

    /// `:y`: keeps `lines` in `register`.
    fn yank_lines(
        &mut self,
        lines: Lines,
        register: Option<char>,
        copy: &BufferCopy,
        shared: &mut Shared,
    ) {
        let (block, ..) = self.lines_text(lines, copy);
        shared
            .registers
            .wrote(register, Register::new(block, true), Wrote::Yank);
        self.report_lines(lines, "lines yanked");
    }

    /// Says how many `lines` a command acted on, if more than Vim's
    /// `report`, as `what`.
    fn report_lines(&mut self, lines: Lines, what: &str) {
        let count = lines.last - lines.first + 1;
        if !self.in_global && count > REPORT as u64 {
            self.status = Some(format!("{count} {what}"));
        }
    }

    /// The byte to put lines at to have them after line `to`, counting
    /// from 1, with 0 for the top, and whether a line break must go before
    /// them there, as after a last line with none.
    fn after_line(&self, to: u64, copy: &BufferCopy) -> (usize, bool) {
        let text = self.text(copy);
        let at = text.line_start(to);
        let len = text.rope().len_bytes();
        let unended = at == len && len > 0 && text.rope().byte(len - 1) != b'\n';
        (at, unended)
    }

    /// `:t`: copies `lines` to after line `to`, counting from 1.
    fn copy_lines(&mut self, lines: Lines, to: u64, copy: &mut BufferCopy) {
        let to = to.min(self.text(copy).lines());
        let (block, ..) = self.lines_text(lines, copy);
        let (at, unended) = self.after_line(to, copy);
        let len = copy.text().len_bytes();
        let inserted = if unended {
            format!("\n{}", block.strip_suffix('\n').unwrap_or(&block))
        } else {
            block
        };
        let mut edit = Edit::new();
        edit.retain(at).insert(&inserted).retain(len - at);
        self.make(edit, copy);
        let count = lines.last - lines.first + 1;
        self.cursor_to_line(to + count - 1, copy);
        self.report_lines(lines, "lines copied");
    }

    /// `:m`: moves `lines` to after line `to`, counting from 1.
    fn move_lines(&mut self, lines: Lines, to: u64, copy: &mut BufferCopy) -> Result<(), String> {
        let count = lines.last - lines.first + 1;
        let to = to.min(self.text(copy).lines());
        // `to` counts from 1, `first` and `last` from 0.
        if to > lines.first && to <= lines.last {
            return Err("lines can't be moved into themselves".into());
        }
        if to == lines.first || to == lines.last + 1 {
            self.cursor_to_line(lines.last, copy);
            return Ok(());
        }
        let (block, start, end, unended) = self.lines_text(lines, copy);
        let (at, at_unended) = self.after_line(to, copy);
        let len = copy.text().len_bytes();
        let mut edit = Edit::new();
        if at < start {
            // Up: the last line, with no break after it, takes the one
            // before it with it.
            let (start, removed) = if unended {
                (start - 1, format!("\n{}", &block[..block.len() - 1]))
            } else {
                (start, block.clone())
            };
            edit.retain(at)
                .insert(&block)
                .retain(start - at)
                .delete(&removed);
            edit.retain(len - (start + removed.len()));
            self.make(edit, copy);
            self.cursor_to_line(to + count - 1, copy);
        } else {
            let inserted = if at_unended {
                format!("\n{}", &block[..block.len() - 1])
            } else {
                block.clone()
            };
            edit.retain(start)
                .delete(&block)
                .retain(at - end)
                .insert(&inserted);
            edit.retain(len - at);
            self.make(edit, copy);
            self.cursor_to_line(to - 1, copy);
        }
        self.report_lines(lines, "lines moved");
        Ok(())
    }

    /// Puts the cursor on `line`'s first character that isn't blank.
    fn cursor_to_line(&mut self, line: u64, copy: &BufferCopy) {
        let text = self.text(copy);
        self.cursor = Cursor::new(Point {
            line: line.min(text.last_line()),
            col: 0,
        });
        self.cursor.apply(Motion::FirstNonBlank, &text);
    }

    // --- Case ---------------------------------------------------------------

    /// Changes the case of the text from byte `start` to `end`, as
    /// `recase` says, as one edit of only what changes, so marks in the
    /// rest stay put.
    pub(super) fn recase(
        &mut self,
        start: usize,
        end: usize,
        recase: Recase,
        copy: &mut BufferCopy,
    ) {
        let was = self.text(copy).slice(start, end);
        let len = copy.text().len_bytes();
        let now = recase.text(&was);
        // Each character stays one, so the two go character by character.
        let pairs: Vec<((usize, char), (usize, char))> =
            was.char_indices().zip(now.char_indices()).collect();
        let Some(first) = pairs.iter().position(|((_, a), (_, b))| a != b) else {
            return;
        };
        let last = pairs
            .iter()
            .rposition(|((_, a), (_, b))| a != b)
            .unwrap_or(first);
        let ((was_start, _), (now_start, _)) = pairs[first];
        let ((was_last, was_char), (now_last, now_char)) = pairs[last];
        let removed = &was[was_start..was_last + was_char.len_utf8()];
        let inserted = &now[now_start..now_last + now_char.len_utf8()];
        let mut edit = Edit::new();
        edit.retain(start + was_start)
            .delete(removed)
            .insert(inserted)
            .retain(len - (start + was_start + removed.len()));
        self.make(edit, copy);
    }
}

/// How many matches of `pattern` `:s` would replace in `bytes` of `hay`,
/// every one on a line if `all`, and on how many lines.
fn count_matches(hay: &str, pattern: &Pattern, bytes: Bytes, all: bool) -> (usize, usize) {
    let (mut count, mut on) = (0, 0);
    let mut last_start: Option<usize> = None;
    substitute::each_match(hay, pattern, bytes, (all, false), &mut |found, _| {
        count += 1;
        on += usize::from(last_start.is_none_or(|last| hay[last..found.start].contains('\n')));
        last_start = Some(found.start);
        true
    });
    (count, on)
}

/// What `rest`, after the range, names, and what follows: `:g!` is `:v`.
pub(super) fn name_of(rest: &str) -> Option<(Name, &str)> {
    let (name, bang, after) = command_name(rest).ok()?;
    Some((if bang { Name::Vglobal } else { name }, after))
}

/// What `rest`, after the range, names, whether it has `!` after it, and
/// what follows.
fn command_name(rest: &str) -> Result<(Name, bool, &str), String> {
    if let Some(after) = rest.strip_prefix('&') {
        return Ok((Name::Again, false, after));
    }
    if let Some(after) = rest.strip_prefix('~') {
        return Ok((Name::AgainSearched, false, after));
    }
    let len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    let word = &rest[..len];
    let Some(name) = named(word) else {
        return Err(if word.is_empty() {
            "a range needs a command after it, such as s, g, d, m or t".into()
        } else {
            format!(
                "{word} isn't a command editors know; they know s, &, ~, g, v, d, y, m, t, normal and format"
            )
        });
    };
    let after = &rest[len..];
    match after.strip_prefix('!') {
        Some(after) if name == Name::Global => Ok((name, true, after)),
        // `:normal!` types no bindings, nor does `:normal`.
        Some(after) if name == Name::Normal => Ok((name, false, after)),
        _ => Ok((name, false, after)),
    }
}

/// Whether `text` starts with an address.
fn starts_address(text: &str) -> bool {
    text.starts_with(|ch: char| ch.is_ascii_digit() || ".$'/?\\+-".contains(ch))
}

/// `text` up to the first `delimiter` without a backslash, and what's
/// after it, or the rest if there's none.
pub(super) fn until(text: &str, delimiter: char) -> (String, &str) {
    let mut part = String::new();
    let mut chars = text.char_indices();
    while let Some((at, ch)) = chars.next() {
        if ch == delimiter {
            return (part, &text[at + ch.len_utf8()..]);
        }
        part.push(ch);
        if ch == '\\'
            && let Some((_, next)) = chars.next()
        {
            part.push(next);
        }
    }
    (part, "")
}

/// Where to look for the next match after `found`: past it, or the next
/// character if it's empty.
fn next_from(text: &str, found: Found) -> usize {
    if found.end > found.start {
        return found.end;
    }
    found.end + text[found.end..].chars().next().map_or(1, char::len_utf8)
}

/// Where each of `lines` of `text` that has a match of `pattern` starting
/// on it starts, or each that hasn't if `invert`.
fn matching_lines(
    hay: &str,
    pattern: &Pattern,
    text: &Text<'_>,
    lines: Lines,
    invert: bool,
) -> Vec<usize> {
    let mut matched = Vec::new();
    let last = lines.last.min(text.last_line());
    let mut line = lines.first;
    while line <= last {
        let start = text.line_start(line).min(hay.len());
        let Some(found) = pattern.find_at(hay, start) else {
            break;
        };
        let on = text.rope().byte_to_line(found.start) as u64;
        if on > last {
            break;
        }
        matched.push(on);
        line = on + 1;
    }
    if !invert {
        return matched
            .into_iter()
            .map(|line| text.line_start(line))
            .collect();
    }
    let mut matched = matched.into_iter().peekable();
    (lines.first..=last)
        .filter(|&line| {
            while matched.next_if(|&on| on < line).is_some() {}
            matched.peek() != Some(&line)
        })
        .map(|line| text.line_start(line))
        .collect()
}
