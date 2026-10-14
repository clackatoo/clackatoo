//! What `:s` would do, shown in place as it's typed, as Neovim's
//! `inccommand=nosplit` shows it, and the matches of `:g`'s pattern.
//!
//! The preview is drawn over the text, never made to it: the window's copy
//! of the buffer isn't touched, so nothing can reach the daemon, other
//! windows, the undo history, the marks or the language server, and
//! there's nothing to put back on Esc. The editor draws from a second
//! rope, the copy's (which costs nothing to clone, as ropes share their
//! pieces) with the replacements in view made to it, and moves the
//! highlights and diagnostics drawn onto it through the changes. The
//! replacements are lit as a search's match is.
//!
//! Only the lines in view are worked out, so typing `:%s` in a long file
//! costs as little as in a short one. If no match is in view, the view
//! moves to the first after the cursor, and back when the preview goes.

use clackatoo_text::{Diagnostic, Diagnostics, Edit, Highlights, Rope, Span};

use super::ex::{self, Lines};
use super::pattern::Pattern;
use super::substitute::{self, Bytes, Flags, Replacement};
use super::wrap::Top;
use super::{BufferCopy, Editor, Shared, Text};
use crate::motion::Point;

/// A command being typed, shown in place.
#[derive(Debug)]
pub(crate) struct Preview {
    /// The command line it's for, and the revision of the copy's text it
    /// was worked out from.
    line: String,
    revision: u64,
    /// The text with the replacements in view made, if a replacement has
    /// been begun; or else, the matches are lit in the text as it is.
    pub(super) text: Option<Rope>,
    /// Each replacement, or match, in order: where it is in the text, and
    /// where it is in the preview's.
    changes: Vec<Change>,
    /// The view's top before the preview moved it to show a match.
    top_was: Option<Top>,
    /// What's lit, in the text drawn, as laid out last.
    pub(super) lit: Vec<(Point, Point)>,
}

/// A replacement, or match, shown.
#[derive(Clone, Copy, Debug)]
struct Change {
    old: (usize, usize),
    new: (usize, usize),
}

impl Preview {
    /// Where byte `old` of the text is in the preview's. One in what's
    /// replaced goes to the replacement's start, or if `after`, its end.
    fn new_of(&self, old: usize, after: bool) -> usize {
        place(&self.changes, old, after, |change| (change.old, change.new))
    }

    /// Where byte `new` of the preview's text is in the text, as
    /// [`new_of`](Self::new_of) goes the other way.
    pub(super) fn old_of(&self, new: usize, after: bool) -> usize {
        place(&self.changes, new, after, |change| (change.new, change.old))
    }

    /// The highlights of the preview's text from byte `start` to `end`,
    /// those of the text moved onto it.
    pub(super) fn spans(&self, all: &Highlights, start: usize, end: usize) -> Vec<Span> {
        let (from, to) = (self.old_of(start, false), self.old_of(end, true));
        (all.within(from, to).iter())
            .filter_map(|span| {
                let (start, end) = (self.new_of(span.start, true), self.new_of(span.end, false));
                (start < end).then_some(Span {
                    start,
                    end,
                    ..*span
                })
            })
            .collect()
    }

    /// The diagnostics of the preview's text from byte `start` to `end`,
    /// as [`spans`](Self::spans) moves highlights.
    pub(super) fn diagnostics(&self, all: &Diagnostics, start: usize, end: usize) -> Diagnostics {
        let (from, to) = (self.old_of(start, false), self.old_of(end, true));
        let moved = (all.within(from, to))
            .map(|diagnostic| {
                let start = self.new_of(diagnostic.start, false);
                let end = self.new_of(diagnostic.end, true).max(start);
                Diagnostic {
                    start,
                    end,
                    ..diagnostic.clone()
                }
            })
            .collect();
        Diagnostics::of(moved)
    }

    /// Lights each replacement, or match, in `text`, the text drawn: the
    /// preview's, or with no replacement begun, the buffer's. One that
    /// leaves nothing isn't lit.
    pub(super) fn light(&mut self, text: &Text<'_>) {
        let rope = text.rope();
        self.lit = (self.changes.iter())
            .map(|change| change.new)
            .filter(|(start, end)| start < end && *end <= rope.len_bytes())
            .map(|(start, end)| {
                let last = rope.char_to_byte(rope.byte_to_char(end) - 1);
                (text.point_of(start), text.point_of(last))
            })
            .collect();
    }
}

/// Where `byte` goes through `changes`, each read as from one place to
/// another by `ends`.
fn place(
    changes: &[Change],
    byte: usize,
    after: bool,
    ends: impl Fn(&Change) -> ((usize, usize), (usize, usize)),
) -> usize {
    // The changes wholly before it, ending before it, or at it if they
    // take anything away.
    let index = changes.partition_point(|change| {
        let ((start, end), _) = ends(change);
        end < byte || (end == byte && start < byte)
    });
    if let Some(change) = changes.get(index) {
        let ((start, _), (to_start, to_end)) = ends(change);
        if start <= byte {
            return if after { to_end } else { to_start };
        }
    }
    match index.checked_sub(1).map(|index| ends(&changes[index])) {
        Some(((_, end), (_, to_end))) => byte - end + to_end,
        None => byte,
    }
}

impl Editor {
    /// Shows what `line`, being typed on the command line, would do, if
    /// it's `:s` or `:g`, or shows nothing if it's `None`, or nothing it
    /// can show. Nothing is done to the text.
    pub(crate) fn preview(&mut self, line: Option<&str>, copy: &BufferCopy, shared: &Shared) {
        let line = line.filter(|_| self.inccommand && copy.loaded());
        let Some(line) = line else {
            self.end_preview();
            return;
        };
        if let Some(preview) = &self.preview
            && preview.line == line
            && preview.revision == copy.revision()
        {
            return;
        }
        // Worked out afresh from the view as it was.
        let top_was = self.preview.take().and_then(|preview| preview.top_was);
        if let Some(top) = top_was {
            self.top = top;
        }
        let (search, cursor) = (self.search.clone(), self.cursor);
        let preview = self.work_out_preview(line, copy, shared);
        self.search = search;
        self.cursor = cursor;
        self.preview = preview.map(|mut preview| {
            preview.top_was = preview.top_was.or(top_was);
            preview
        });
        if self.preview.is_none()
            && let Some(top) = top_was
        {
            self.top = top;
        }
    }

    /// Stops showing a preview, putting the view back where it was.
    pub(super) fn end_preview(&mut self) {
        if let Some(top) = self.preview.take().and_then(|preview| preview.top_was) {
            self.top = top;
        }
    }

    /// What `line` would do, if it's `:s` or `:g` and can be shown.
    fn work_out_preview(
        &mut self,
        line: &str,
        copy: &BufferCopy,
        shared: &Shared,
    ) -> Option<Preview> {
        let (lines, pattern, flags, replacement) = self.typed_command(line, copy, shared)?;
        let last_line = self.text(copy).last_line();
        let compiled = self.compile(&pattern, flags.exact, copy, shared).ok()?;
        let with = match &replacement {
            Some(replacement) => Some(Replacement::new(replacement).ok()?),
            None => None,
        };
        let (whole, len) = self.haystack(copy);
        let hay = &whole[..len];
        let mut top_was = None;
        let mut shown = self.lines_in_view(lines, last_line);
        let in_view = shown.is_some_and(|shown| self.matches_in(hay, &compiled, shown, copy));
        if !in_view {
            // To the first match after the cursor, or the first at all.
            let line = self.first_matching(hay, &compiled, lines, copy)?;
            top_was = Some(self.top);
            let height = self.height.max(1) as u64;
            self.top = Top {
                line: line.saturating_sub(height / 2),
                cell: 0,
            };
            shown = self.lines_in_view(lines, last_line);
        }
        let shown = shown?;
        let text = self.text(copy);
        let bytes = Bytes {
            start: text.line_start(shown.first),
            end: text.line_end(shown.last).min(len),
        };
        drop(text);
        let mut changes = Vec::new();
        let mut edit = Edit::new();
        let (mut through, mut longer) = (0, 0isize);
        let mut expanded = String::new();
        // A pattern alone lights every match, as a search does.
        let all = flags.all || with.is_none();
        let groups = with.as_ref().is_some_and(Replacement::uses_groups);
        substitute::each_match(
            hay,
            &compiled,
            bytes,
            (all, groups),
            &mut |found, locations| {
                let old = (found.start, found.end);
                match &with {
                    Some(with) => {
                        expanded.clear();
                        with.expand(hay, &compiled, (found, locations), &mut expanded);
                        edit.retain(found.start - through)
                            .delete(&hay[found.start..found.end])
                            .insert(&expanded);
                        through = found.end;
                        let start = found.start.saturating_add_signed(longer);
                        longer +=
                            expanded.len().cast_signed() - (found.end - found.start).cast_signed();
                        changes.push(Change {
                            old,
                            new: (start, start + expanded.len()),
                        });
                    }
                    None => changes.push(Change { old, new: old }),
                }
                true
            },
        );
        let text = with.is_some().then(|| {
            edit.retain(whole.len() - through);
            let mut rope = copy.text().clone();
            if edit.apply(&mut rope).is_err() {
                log::error!("a preview's edit didn't fit");
            }
            rope
        });
        Some(Preview {
            line: line.to_owned(),
            revision: copy.revision(),
            text,
            changes,
            top_was,
            lit: Vec::new(),
        })
    }

    /// What `line` asks for, if it's `:s` or `:g` as far as typed: the
    /// lines, the pattern, the flags, and the replacement, if begun.
    fn typed_command(
        &mut self,
        line: &str,
        copy: &BufferCopy,
        shared: &Shared,
    ) -> Option<(Lines, String, Flags, Option<String>)> {
        let mut rest = line.trim_start_matches([' ', ':']);
        if !ex::is_editor_command(rest) {
            return None;
        }
        let range = self.range(&mut rest, copy, shared).ok()?;
        let (name, after) = ex::name_of(rest.trim_start())?;
        let last_line = self.text(copy).last_line();
        let current = self.cursor.at.line;
        Some(match name {
            ex::Name::Substitute => {
                let typed = substitute::read(after).ok()?;
                let last_flags = shared
                    .substituted
                    .as_ref()
                    .map(|last| last.flags)
                    .unwrap_or_default();
                let flags = Flags::read(&typed.flags, last_flags).ok()?;
                let pattern = self.pattern_or_last(typed.pattern.as_deref()?).ok()?;
                let replacement = typed.replacing.then(|| {
                    let previous = shared
                        .substituted
                        .as_ref()
                        .map_or("", |last| last.replacement.as_str());
                    substitute::expand_tilde(&typed.replacement, previous)
                });
                let lines = range.unwrap_or(Lines {
                    first: current,
                    last: current,
                    typed: false,
                });
                (lines, pattern, flags, replacement)
            }
            ex::Name::Global | ex::Name::Vglobal => {
                let after = after.strip_prefix('!').unwrap_or(after).trim_start();
                let delimiter = after.chars().next()?;
                let (pattern, _) = ex::until(&after[delimiter.len_utf8()..], delimiter);
                let pattern = self.pattern_or_last(&pattern).ok()?;
                let lines = range.unwrap_or(Lines {
                    first: 0,
                    last: last_line,
                    typed: true,
                });
                let flags = Flags {
                    all: true,
                    ..Flags::default()
                };
                (lines, pattern, flags, None)
            }
            _ => return None,
        })
    }

    /// The lines of `lines` in view, if any are.
    fn lines_in_view(&self, lines: Lines, last_line: u64) -> Option<Lines> {
        let top = self.top.line;
        let bottom = (top + self.height.max(1) as u64 - 1).min(last_line);
        let first = lines.first.max(top);
        let last = lines.last.min(bottom);
        (first <= last).then_some(Lines {
            first,
            last,
            typed: true,
        })
    }

    /// Whether `pattern` matches in `lines` of `hay`, the copy's text.
    fn matches_in(&self, hay: &str, pattern: &Pattern, lines: Lines, copy: &BufferCopy) -> bool {
        let text = self.text(copy);
        let start = text.line_start(lines.first).min(hay.len());
        let end = text.line_end(lines.last).min(hay.len());
        pattern
            .find_at(hay, start)
            .is_some_and(|found| found.start <= end)
    }

    /// The first line of `lines` from the cursor's on with a match of
    /// `pattern` starting on it, or else from their start.
    fn first_matching(
        &self,
        hay: &str,
        pattern: &Pattern,
        lines: Lines,
        copy: &BufferCopy,
    ) -> Option<u64> {
        let text = self.text(copy);
        let end = text.line_end(lines.last).min(hay.len());
        let from = self.cursor.at.line.clamp(lines.first, lines.last);
        [from, lines.first].into_iter().find_map(|line| {
            let start = text.line_start(line).min(hay.len());
            (pattern.find_at(hay, start))
                .filter(|found| found.start <= end)
                .map(|found| text.rope().byte_to_line(found.start) as u64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_go_through_the_changes_both_ways() {
        // "ab cd ef": "cd" replaced by "XYZ", and "" put in before "ef".
        let changes = [
            Change {
                old: (3, 5),
                new: (3, 6),
            },
            Change {
                old: (6, 6),
                new: (7, 8),
            },
        ];
        let new = |old, after| place(&changes, old, after, |change| (change.old, change.new));
        let old = |new, after| place(&changes, new, after, |change| (change.new, change.old));
        assert_eq!(
            [new(0, false), new(3, false), new(3, true), new(4, true)],
            [0, 3, 6, 6]
        );
        assert_eq!(
            [new(5, false), new(6, false), new(6, true), new(8, false)],
            [6, 7, 8, 10]
        );
        assert_eq!(
            [old(4, false), old(4, true), old(6, false), old(9, false)],
            [3, 5, 5, 7]
        );
    }
}
