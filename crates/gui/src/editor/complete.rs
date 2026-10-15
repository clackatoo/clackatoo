//! Completion in insert mode: what the language server suggests could be
//! typed to finish the word before the cursor, narrowed down and ranked as
//! more is typed, in a menu under it.
//!
//! The server is asked once, and its items are kept. Each key typed after
//! that only narrows them down again here, matching the word typed so far
//! against each item fuzzily, as fzf does, unless the server said its
//! answer wasn't all there is, when it's asked again too. The items' places
//! are kept as they came, and moved through the edits made since only for
//! the item accepted, so typing never costs a pass over thousands of them.
//!
//! A snippet is written out as it's put in (see [`clackatoo_text::Snippet`])
//! and the places to type in it are handed on, for the editor to go from
//! one to the next (see [`super::snippet`]). The same menu shows a
//! place's choices, when it has them.

use std::collections::HashMap;

use clackatoo_protocol::{CompletionItem, Edit};
use clackatoo_text::{Bias, Expanded, Rope, Snippet, Stop};
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32String};

use super::panel::{Line, Panel, Placed, Run};
use crate::render::{Ink, mono_cells, mono_width};

/// The most items the menu shows at once. More are scrolled to.
const MENU_HEIGHT: usize = 10;

/// The widest the menu goes, in cells: what's shown beside an item's label
/// is cut to fit.
const MENU_WIDTH: usize = 60;

/// The cells an item's kind takes, before its label, with the gap after.
const KIND_WIDTH: usize = 8;

/// The narrowest the documentation beside the menu is worth showing at.
const LEAST_DOCS: usize = 24;

/// The widest the documentation beside the menu goes, in cells.
const DOCS_WIDTH: usize = 60;

/// The most bytes before the word an item is put in place of are looked
/// back over, for the start of what it names (see [`already_there`]).
const MOST_BEFORE: usize = 256;

/// Completion items ranked against what's typed: those that match it,
/// best first, by how well they match, then as the language server sorts
/// them. Public for its benchmark.
#[derive(Debug)]
pub struct Ranking {
    matcher: Matcher,
    /// Each item's text to match, as the matcher reads it.
    haystacks: Vec<Utf32String>,
    /// Each item's place in the server's order: those it would have picked
    /// first, then by their sort text.
    order: Vec<u32>,
    /// What was typed when last ranked.
    query: String,
    /// The items that match it, best first, by index.
    shown: Vec<u32>,
}

impl Ranking {
    /// Ranks items, each given as the text to match it by, the text to sort
    /// it by, and whether the server would have it picked first; all of
    /// them shown, in the server's order, until [`narrow`](Self::narrow)
    /// says what's typed.
    pub fn new<'a>(items: impl IntoIterator<Item = (&'a str, &'a str, bool)>) -> Self {
        let mut haystacks = Vec::new();
        let mut keys: Vec<(bool, &str)> = Vec::new();
        for (filter, sort, preselect) in items {
            haystacks.push(Utf32String::from(filter));
            keys.push((!preselect, sort));
        }
        let mut by_order: Vec<u32> = (0..index(keys.len())).collect();
        by_order.sort_by(|&a, &b| keys[a as usize].cmp(&keys[b as usize]));
        let mut order = vec![0; keys.len()];
        for (place, &item) in (0..).zip(&by_order) {
            order[item as usize] = place;
        }
        let mut config = Config::DEFAULT;
        // What's typed is mostly the start of what's wanted.
        config.prefer_prefix = true;
        Self {
            matcher: Matcher::new(config),
            haystacks,
            order,
            query: String::new(),
            shown: by_order,
        }
    }

    /// Ranks the items against `query`, what's typed. When it's what was
    /// typed before with more after, only those that matched that are
    /// looked at, as nothing else can match it.
    pub fn narrow(&mut self, query: &str) {
        if query == self.query {
            return;
        }
        let looked_at: Vec<u32> = if !self.query.is_empty() && query.starts_with(&self.query) {
            std::mem::take(&mut self.shown)
        } else {
            (0..index(self.haystacks.len())).collect()
        };
        query.clone_into(&mut self.query);
        if query.is_empty() {
            let mut all = looked_at;
            all.sort_by_key(|&index| self.order[index as usize]);
            self.shown = all;
            return;
        }
        let atom = atom(query);
        let typed: Vec<char> = query.chars().collect();
        let mut scored: Vec<(bool, u16, u32, u32)> = Vec::with_capacity(looked_at.len());
        for index in looked_at {
            let haystack = self.haystacks[index as usize].slice(..);
            if let Some(score) = atom.score(haystack, &mut self.matcher) {
                // What starts with exactly what's typed, case and all,
                // comes before what only matches it.
                let prefix = haystack.len() >= typed.len()
                    && haystack.chars().zip(&typed).all(|(ch, typed)| ch == *typed);
                scored.push((prefix, score, self.order[index as usize], index));
            }
        }
        // Best first, then as the server sorts them.
        scored.sort_unstable_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)).then(a.2.cmp(&b.2)));
        self.shown = scored.into_iter().map(|(.., index)| index).collect();
    }

    /// The items matching what's typed, best first, by index.
    #[must_use]
    pub fn shown(&self) -> &[u32] {
        &self.shown
    }

    /// The characters of `text` that match what's typed, by index, as
    /// they're highlighted in the menu.
    fn matched(&mut self, text: &str) -> Vec<u32> {
        let mut indices = Vec::new();
        if self.query.is_empty() {
            return indices;
        }
        let mut chars = Vec::new();
        let haystack = nucleo_matcher::Utf32Str::new(text, &mut chars);
        atom(&self.query).indices(haystack, &mut self.matcher, &mut indices);
        indices.sort_unstable();
        indices.dedup();
        indices
    }
}

/// `count`, as an item's index: there are never more than fit.
fn index(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// `query` as the matcher reads it: fuzzily, ignoring case unless it has
/// a capital.
fn atom(query: &str) -> Atom {
    Atom::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    )
}

/// What the language server says of an item when it's resolved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub(crate) detail: Option<String>,
    pub(crate) documentation: Option<(String, bool)>,
}

/// The word before the cursor being completed, with the items the
/// language server suggested for it.
#[derive(Debug)]
pub(crate) struct Completion {
    /// The number of the query they answered, to resolve one by.
    pub(crate) list: u64,
    /// Where the word starts, moved along with every edit.
    pub(crate) start: usize,
    items: Vec<CompletionItem>,
    ranking: Ranking,
    /// Whether the server said they weren't all there are, so typing more
    /// asks again.
    pub(crate) incomplete: bool,
    /// How long the word was when the server was asked, in bytes: one
    /// shorter now needs asking again, as the server may have left out what
    /// it didn't start with.
    pub(crate) asked: usize,
    /// The item picked, as a place in those shown, and whether it was
    /// moved to with a key, rather than being the first.
    selected: usize,
    pub(crate) moved: bool,
    /// The first of those shown that's in view.
    top: usize,
    /// The edits made since the items came, which their places are moved
    /// through when one is accepted.
    since: Vec<Edit>,
    /// What's been said of items when resolved, by index, and the query
    /// asking about one, and which.
    resolved: HashMap<u32, Resolved>,
    pub(crate) resolving: Option<(u64, u32)>,
    /// The selected item's documentation, beside the menu.
    docs: Option<Panel>,
    /// The menu's lines in view, and where it goes, as last settled.
    lines: Vec<Line>,
    placed: Option<Placed>,
    /// Whether the items are a snippet's choices for a place, which no
    /// server is asked about, and typing doesn't narrow down.
    pub(crate) choices: bool,
}

/// What accepting an item does: the edit, and where the cursor goes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Accepted {
    pub(crate) edit: Edit,
    pub(crate) cursor: usize,
    /// What's put in where the word was, and how far into it the cursor
    /// goes, for `.` to type again.
    pub(crate) text: String,
    pub(crate) offset: usize,
    /// How many characters of the word typed it replaces, before the
    /// cursor.
    pub(crate) replaced: usize,
    /// Where what's put in starts, in the text after the edit.
    pub(crate) at: usize,
    /// The places to type in it, if it's a snippet, from where it starts.
    pub(crate) stops: Vec<Stop>,
}

impl Completion {
    /// The items answered to the completion query numbered `list`, for the
    /// word starting at `start`.
    pub(crate) fn new(
        list: u64,
        start: usize,
        items: Vec<CompletionItem>,
        incomplete: bool,
    ) -> Self {
        let ranking = Ranking::new(items.iter().map(|item| {
            (
                item.filter.as_deref().unwrap_or(&item.label),
                item.sort.as_deref().unwrap_or(&item.label),
                item.preselect,
            )
        }));
        Self {
            list,
            start,
            items,
            ranking,
            incomplete,
            asked: 0,
            selected: 0,
            moved: false,
            top: 0,
            since: Vec::new(),
            resolved: HashMap::new(),
            resolving: None,
            docs: None,
            lines: Vec::new(),
            placed: None,
            choices: false,
        }
    }

    /// A snippet's `choices` for the place starting at `start`, to pick
    /// from in its place, in the order they're written.
    pub(crate) fn choices(start: usize, choices: &[String]) -> Self {
        let items = (choices.iter())
            .map(|choice| CompletionItem {
                label: choice.clone(),
                kind: 12,
                detail: String::new(),
                filter: None,
                sort: None,
                preselect: false,
                start: Some(start),
                text: choice.clone(),
                snippet: false,
                more: Vec::new(),
            })
            .collect();
        let mut completion = Self::new(0, start, items, false);
        completion.ranking.shown.sort_unstable();
        completion.choices = true;
        completion
    }

    /// Narrows the items down to those matching `query`, the word typed so
    /// far, picking the first again if the items shown change.
    pub(crate) fn narrow(&mut self, query: &str) {
        let before = self.ranking.shown().first().copied();
        let count = self.ranking.shown().len();
        self.ranking.narrow(query);
        if self.ranking.shown().first().copied() != before || self.ranking.shown().len() != count {
            self.selected = 0;
            self.top = 0;
            self.moved = false;
        }
    }

    /// Whether any item matches what's typed, so the menu shows.
    pub(crate) fn showing(&self) -> bool {
        !self.ranking.shown().is_empty()
    }

    /// Moves the place `place`, a byte of the text when the items came,
    /// through the edits made since.
    fn now(&self, place: usize) -> usize {
        (self.since.iter()).fold(place, |place, edit| edit.map(place, Bias::Before))
    }

    /// Takes in `edit`, made to the text: the word's start moves with it,
    /// and the items' places will.
    pub(crate) fn edited(&mut self, edit: &Edit) {
        self.start = edit.map(self.start, Bias::Before);
        self.since.push(edit.clone());
    }

    /// Moves the pick to the next item, or the one before if not `down`,
    /// going round the ends.
    pub(crate) fn pick(&mut self, down: bool) {
        let count = self.ranking.shown().len();
        if count == 0 {
            return;
        }
        self.selected = if down {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        self.moved = true;
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + MENU_HEIGHT {
            self.top = self.selected + 1 - MENU_HEIGHT;
        }
    }

    /// The index of the item picked, if any's shown.
    pub(crate) fn picked(&self) -> Option<u32> {
        self.ranking.shown().get(self.selected).copied()
    }

    /// The label of the item picked, if one was moved to with a key.
    pub(crate) fn moved_to(&self) -> Option<&str> {
        let index = self.picked().filter(|_| self.moved)?;
        Some(&self.items[index as usize].label)
    }

    /// Picks the item labelled `label`, as if moved to, if it's shown: so
    /// a list that replaces another keeps what was picked in it.
    pub(crate) fn pick_label(&mut self, label: &str) {
        let shown = self.ranking.shown();
        let Some(place) =
            (shown.iter()).position(|&index| self.items[index as usize].label == label)
        else {
            return;
        };
        self.selected = place;
        self.moved = true;
        if self.selected < self.top || self.selected >= self.top + MENU_HEIGHT {
            self.top = self.selected.saturating_sub(MENU_HEIGHT - 1);
        }
    }

    /// The item at `index`.
    pub(crate) fn item(&self, index: u32) -> Option<&CompletionItem> {
        self.items.get(index as usize)
    }

    /// What's been said of item `index` when resolved, if it has been.
    pub(crate) fn resolved(&self, index: u32) -> Option<&Resolved> {
        self.resolved.get(&index)
    }

    /// Takes in what the server said of item `index`, resolved.
    pub(crate) fn resolve(&mut self, index: u32, resolved: Resolved) {
        self.resolved.insert(index, resolved);
    }

    /// Shows the documentation of the item picked beside the menu, from
    /// what's in the list and what's been said of it resolved: its detail,
    /// as code, then its documentation. None if there's none to show.
    pub(crate) fn document(&mut self, tabstop: usize) {
        let Some(index) = self.picked() else {
            self.docs = None;
            return;
        };
        let item = &self.items[index as usize];
        let resolved = self.resolved.get(&index);
        let detail = (resolved.and_then(|resolved| resolved.detail.as_deref()))
            .filter(|detail| !detail.is_empty())
            .or(Some(item.detail.as_str()).filter(|detail| !detail.is_empty()));
        let documentation = resolved.and_then(|resolved| resolved.documentation.as_ref());
        self.docs = Panel::documentation(
            detail,
            documentation.map(|(text, markdown)| (text.as_str(), *markdown)),
            tabstop,
        );
    }

    /// What accepting the item picked does, the cursor being at byte
    /// `cursor` of `text`: what's from where it starts to the cursor is
    /// replaced with its text, a snippet's as `expand` writes it out, and
    /// its edits elsewhere are made with it, but any that would overlap
    /// that. `None` if none is picked; an error, saying why, if its edits
    /// make no sense.
    pub(crate) fn accept(
        &self,
        text: &Rope,
        cursor: usize,
        expand: impl FnOnce(&Snippet) -> Expanded,
    ) -> Option<Result<Accepted, String>> {
        let item = self.item(self.picked()?)?;
        let line_start = text.line_to_byte(text.byte_to_line(cursor.min(text.len_bytes())));
        let said = (item.start.map(|start| self.now(start)))
            .filter(|&start| start <= cursor && start >= line_start);
        let (mut put, mut offset, mut stops) = if item.snippet {
            let Expanded { text, stops } = expand(&Snippet::parse(&item.text));
            // The cursor goes to the end of the first place to type, or
            // else to the last, `$0`.
            let first = (stops.iter())
                .filter(|stop| stop.number > 0 && !stop.mirror)
                .min_by_key(|stop| stop.number)
                .map(|stop| stop.end);
            let last = (stops.iter()).find(|stop| stop.number == 0 && !stop.mirror);
            let offset = first.or(last.map(|stop| stop.start)).unwrap_or(text.len());
            (text, offset, stops)
        } else {
            (item.text.clone(), item.text.len(), Vec::new())
        };
        // Where the server said, if it's not past the cursor or on a line
        // before, or else where the word starts. What's before the word
        // that the text starts with is already there: only the rest goes
        // in, so what's replaced is what was typed, as `.` types it again.
        let start = said.unwrap_or_else(|| {
            let start = self.start.min(cursor);
            let there = already_there(text, start, cursor, line_start, &put);
            if there > 0 && offset >= there && stops.iter().all(|stop| stop.start >= there) {
                put.drain(..there);
                offset -= there;
                for stop in &mut stops {
                    stop.start -= there;
                    stop.end -= there;
                }
            }
            start
        });
        let mut changes = vec![Splice {
            start,
            end: cursor,
            text: put.clone(),
            main: true,
        }];
        // Where the cursor goes: as far along as the edits before the word
        // move its start, and into the text put in.
        let mut moved: isize = 0;
        for more in &item.more {
            let (from, to) = (self.now(more.start), self.now(more.end));
            let to = to.max(from);
            if to <= start || from >= cursor.max(start + 1) {
                if to <= start {
                    moved += more.text.len().cast_signed() - (to - from).cast_signed();
                }
                changes.push(Splice {
                    start: from,
                    end: to,
                    text: more.text.clone(),
                    main: false,
                });
            }
        }
        let Some(edit) = splice(text, changes) else {
            return Some(Err("the language server's edits for it don't fit".into()));
        };
        let replaced = text.byte_slice(start..cursor).chars().count();
        let at = start.saturating_add_signed(moved);
        Some(Ok(Accepted {
            cursor: at + offset,
            edit,
            text: put,
            offset,
            replaced,
            at,
            stops,
        }))
    }

    /// Works out the menu's lines and where it goes, and the
    /// documentation's beside it, in a pane `rows` by `cols`, the cursor
    /// being at `cursor` and the word's start at column `word` of it.
    /// Nothing's shown while the cursor's out of view.
    pub(crate) fn settle(
        &mut self,
        (rows, cols): (usize, usize),
        cursor: Option<(usize, usize)>,
        word: usize,
    ) {
        self.placed = None;
        let Some((row, _)) = cursor else {
            return;
        };
        let shown = self.ranking.shown().len();
        if shown == 0 || cols < KIND_WIDTH + 6 {
            return;
        }
        self.top = self.top.min(shown.saturating_sub(MENU_HEIGHT));
        let in_view = self.top..(self.top + MENU_HEIGHT).min(shown);
        let rows_wanted = in_view.len();
        // Below the cursor if it fits, or else above, or else wherever
        // there's more room.
        let below = rows.saturating_sub(row + 1);
        let above = row;
        let (top, height) = if rows_wanted < below {
            (row + 1, rows_wanted)
        } else if rows_wanted < above {
            (row - rows_wanted - 1, rows_wanted)
        } else if below >= above {
            (row + 1, below.saturating_sub(1))
        } else {
            (0, above.saturating_sub(1))
        };
        if height == 0 {
            return;
        }
        let in_view = self.top..self.top + height;
        if self.selected >= in_view.end {
            self.top = self.selected + 1 - height;
        }
        let in_view = self.top..self.top + height;
        let room = (cols - 2).min(MENU_WIDTH);
        let indices: Vec<u32> = self.ranking.shown()[in_view.clone()].to_vec();
        let width = (indices.iter())
            .map(|&index| {
                let item = &self.items[index as usize];
                let detail = mono_width(&item.detail);
                KIND_WIDTH + mono_width(&item.label) + if detail > 0 { 2 + detail } else { 0 }
            })
            .max()
            .unwrap_or(0)
            .min(room);
        self.lines = (indices.iter().enumerate())
            .map(|(offset, &index)| self.line(index, width, self.top + offset == self.selected))
            .collect();
        // The labels line up with the word, as far as the pane allows.
        let boxed = width + 2;
        let left = word.saturating_sub(KIND_WIDTH + 1).min(cols - boxed);
        let placed = Placed {
            top,
            left,
            width,
            height,
        };
        self.placed = Some(placed);
        if let Some(docs) = &mut self.docs {
            docs.settle_beside((rows, cols), placed, top > row, (LEAST_DOCS, DOCS_WIDTH));
        }
    }

    /// The line showing item `index`, `width` cells wide: its kind, its
    /// label with what matches what's typed highlighted, and its detail,
    /// dimmed, as much as fits.
    fn line(&mut self, index: u32, width: usize, picked: bool) -> Line {
        let item = &self.items[index as usize];
        let (label, detail) = (item.label.clone(), item.detail.clone());
        let kind = kind_name(item.kind);
        let matched = self.ranking.matched(&label);
        let mut runs = vec![Run {
            text: format!("{kind:<KIND_WIDTH$}"),
            ink: Ink::Muted,
            code: false,
        }];
        let mut used = KIND_WIDTH;
        for (index, ch) in (0..).zip(label.chars()) {
            let cells = mono_cells(ch);
            if used + cells > width {
                break;
            }
            used += cells;
            let ink = if matched.binary_search(&index).is_ok() {
                Ink::Matched
            } else {
                Ink::Plain
            };
            match runs.last_mut() {
                Some(run) if run.ink == ink => run.text.push(ch),
                _ => runs.push(Run {
                    text: ch.to_string(),
                    ink,
                    code: false,
                }),
            }
        }
        if !detail.is_empty() && used + 3 <= width {
            let mut shown = String::from("  ");
            let room = width - used - 2;
            let mut taken = 0;
            for ch in detail.chars() {
                let cells = mono_cells(ch);
                if taken + cells > room {
                    shown.pop();
                    shown.push('…');
                    break;
                }
                taken += cells;
                shown.push(ch);
            }
            runs.push(Run {
                text: shown,
                ink: Ink::Muted,
                code: false,
            });
        }
        Line {
            runs,
            block: false,
            picked,
        }
    }

    /// The menu as it's to be drawn, if there's room for it.
    pub(crate) fn menu(&self) -> Option<super::PanelView<'_>> {
        let placed = self.placed?;
        Some(super::PanelView {
            placed,
            lines: &self.lines,
            first: self.top,
            total: self.ranking.shown().len(),
        })
    }

    /// The documentation beside the menu, as it's to be drawn, if it's
    /// shown.
    pub(crate) fn docs(&self) -> Option<super::PanelView<'_>> {
        self.placed?;
        self.docs.as_ref()?.view()
    }

    /// Where the menu is, as last settled.
    pub(crate) fn placed(&self) -> Option<Placed> {
        self.placed
    }
}

/// The short name an item's kind is shown by, as LSP numbers them.
fn kind_name(kind: u8) -> &'static str {
    match kind {
        1 => "text",
        2 => "method",
        3 => "fn",
        4 => "new",
        5 => "field",
        6 => "var",
        7 => "class",
        8 => "iface",
        9 => "module",
        10 => "prop",
        11 => "unit",
        12 => "value",
        13 => "enum",
        14 => "keyword",
        15 => "snip",
        16 => "color",
        17 => "file",
        18 => "ref",
        19 => "dir",
        20 => "variant",
        21 => "const",
        22 => "struct",
        23 => "event",
        24 => "op",
        25 => "type",
        _ => "",
    }
}

/// How many bytes of `put`, the text going in place of the word from
/// `start` to `cursor` in `text`, whose line starts at `line_start`, are
/// there already, just before the word: the most of what's before it that
/// `put` starts with, followed by the word. So accepting `libcap-tools`
/// after `libcap-t`, where words have no `-` in them and the server didn't
/// say where the name starts, puts in only `tools` in place of the `t`,
/// not all of it. It's never past a blank, the line's start, or
/// [`MOST_BEFORE`] bytes.
fn already_there(text: &Rope, start: usize, cursor: usize, line_start: usize, put: &str) -> usize {
    // A line broken between the word's start and the cursor, by another
    // window or an agent as the menu was open, has no word to look before.
    if start < line_start {
        return 0;
    }
    let typed = text.byte_slice(start..cursor).to_string();
    let reach = put.len().min(MOST_BEFORE);
    let line = text.byte_slice(line_start..start);
    let mut back = Vec::new();
    let mut len = typed.len();
    for ch in line.chars_at(line.len_chars()).reversed() {
        len += ch.len_utf8();
        if ch.is_whitespace() || len > reach {
            break;
        }
        back.push(ch);
    }
    let before: String = back.into_iter().rev().collect();
    // The longest that fits: `a-b-c` after `a-b-` with `c` typed takes all
    // of `a-b-`, though `b-` alone would fit.
    (before.char_indices())
        .map(|(at, _)| &before[at..])
        .find(|there| put.starts_with(there) && put[there.len()..].starts_with(&typed))
        .map_or(0, str::len)
}

/// A change accepting an item makes: bytes `start` to `end` replaced with
/// `text`, the item's own text if `main`.
#[derive(Debug)]
pub(crate) struct Splice {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) text: String,
    pub(crate) main: bool,
}

/// One edit to `text` making `changes`, given as they are in the text
/// before any is made. At one place, the item's own text goes after
/// what's put there with it, as `&` before a word. `None` if they overlap
/// or aren't in the text.
pub(crate) fn splice(text: &Rope, mut changes: Vec<Splice>) -> Option<Edit> {
    changes.sort_by_key(|change| (change.start, change.end, change.main));
    let len = text.len_bytes();
    let mut edit = Edit::new();
    // How far through the text the edit has gone.
    let mut through = 0;
    for Splice {
        start,
        end,
        text: put,
        ..
    } in changes
    {
        if start < through || end < start || end > len {
            return None;
        }
        let removed = text.get_byte_slice(start..end)?.to_string();
        edit.retain(start - through).delete(&removed).insert(&put);
        through = end;
    }
    edit.retain(len - through);
    Some(edit)
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::TextChange;

    use super::*;

    fn item(label: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_owned(),
            kind: 6,
            detail: String::new(),
            filter: None,
            sort: None,
            preselect: false,
            start: None,
            text: label.to_owned(),
            snippet: false,
            more: Vec::new(),
        }
    }

    fn labels(completion: &Completion) -> Vec<&str> {
        (completion.ranking.shown().iter())
            .map(|&index| completion.items[index as usize].label.as_str())
            .collect()
    }

    #[test]
    fn items_are_narrowed_and_ranked_as_more_is_typed() {
        let mut items: Vec<CompletionItem> = ["print", "println", "eprintln", "sprint", "zzz"]
            .into_iter()
            .map(item)
            .collect();
        // The server sorts sprint first, and would have zzz picked.
        items[3].sort = Some("0".into());
        items[4].preselect = true;
        let mut completion = Completion::new(1, 0, items, false);
        assert_eq!(
            labels(&completion),
            ["zzz", "sprint", "eprintln", "print", "println"]
        );
        completion.narrow("pr");
        assert!(!labels(&completion).contains(&"zzz"));
        assert_eq!(
            labels(&completion)[..2],
            ["print", "println"],
            "prefixes first"
        );
        completion.narrow("prl");
        assert_eq!(labels(&completion), ["println", "eprintln"]);
        // Back to less, all are looked at again.
        completion.narrow("p");
        assert_eq!(labels(&completion).len(), 4);
        completion.narrow("q");
        assert!(!completion.showing());
        // Case matters only with a capital.
        let mut completion = Completion::new(1, 0, vec![item("Vec"), item("vector")], false);
        completion.narrow("vec");
        assert_eq!(
            labels(&completion),
            ["vector", "Vec"],
            "what starts with what's typed, case and all, first"
        );
        completion.narrow("Vec");
        assert_eq!(labels(&completion), ["Vec"]);
    }

    #[test]
    fn the_matched_characters_of_a_label_are_lit() {
        let mut completion = Completion::new(1, 0, vec![item("print_line")], false);
        completion.narrow("pl");
        let line = completion.line(0, 40, true);
        let lit: Vec<&str> = (line.runs.iter())
            .filter(|run| run.ink == Ink::Matched)
            .map(|run| run.text.as_str())
            .collect();
        assert_eq!(lit, ["p", "l"]);
        assert!(line.picked);
        assert_eq!(line.text(), "var     print_line");
    }

    /// `snippet` written out with no variables known, its line breaks
    /// and tabs as they are.
    fn plain(snippet: &Snippet) -> Expanded {
        snippet.expand(|_| None, "\n", "\t")
    }

    #[test]
    fn a_snippet_puts_the_cursor_at_the_end_of_its_first_place() {
        let snippet = |text: &str| {
            let mut item = item("s");
            item.text = text.into();
            item.snippet = true;
            item.start = None;
            item
        };
        assert_eq!(
            accepted("|", snippet("call(${1:a}, ${2:b})$0"), 0),
            "call(a|, b)"
        );
        assert_eq!(accepted("|", snippet("f($2, $1)"), 0), "f(, |)");
        assert_eq!(accepted("|", snippet("f($0)"), 0), "f(|)");
        assert_eq!(accepted("|", snippet("plain"), 0), "plain|");
        // Its places, from where it starts, which is past the word.
        let completion = Completion::new(1, 2, vec![snippet("g(${1:x})")], false);
        let accepted = (completion.accept(&Rope::from_str("> g"), 3, plain))
            .unwrap()
            .unwrap();
        assert_eq!(accepted.at, 2);
        let places: Vec<_> = (accepted.stops.iter())
            .map(|stop| (stop.number, stop.start, stop.end))
            .collect();
        assert_eq!(places, [(1, 2, 3), (0, 4, 4)]);
    }

    #[test]
    fn accepting_replaces_the_word_and_makes_the_edits_elsewhere() {
        let text = Rope::from_str("fn main() {\n    pri\n}\n");
        let start = "fn main() {\n    ".len();
        let cursor = start + 3;
        let mut println = item("println");
        println.text = "println!(${1:fmt})".into();
        println.snippet = true;
        println.start = Some(start);
        println.more = vec![TextChange {
            start: 0,
            end: 0,
            text: "use std::io;\n".into(),
        }];
        let completion = Completion::new(1, start, vec![println], false);
        let accepted = completion.accept(&text, cursor, plain).unwrap().unwrap();
        let after = accepted.edit.apply_to(&text.to_string()).unwrap();
        assert_eq!(after, "use std::io;\nfn main() {\n    println!(fmt)\n}\n");
        assert_eq!(&after[accepted.cursor..], ")\n}\n", "past its first place");
        assert_eq!(accepted.replaced, 3);

        // Places move through edits made since; one overlapping the word
        // is left out.
        let mut overlapping = item("x");
        overlapping.start = Some(start);
        overlapping.more = vec![TextChange {
            start: start + 1,
            end: start + 2,
            text: "!".into(),
        }];
        let mut completion = Completion::new(1, start, vec![overlapping], false);
        let typed = Edit::replace(text.len_bytes(), 0, "", "// \n");
        completion.edited(&typed);
        let text = Rope::from_str(&typed.apply_to(&text.to_string()).unwrap());
        let accepted = completion
            .accept(&text, cursor + 4, plain)
            .unwrap()
            .unwrap();
        assert_eq!(
            accepted.edit.apply_to(&text.to_string()).unwrap(),
            "// \nfn main() {\n    x\n}\n"
        );
        let cut = |start, end| Splice {
            start,
            end,
            text: String::new(),
            main: false,
        };
        assert_eq!(splice(&text, vec![cut(1, 3), cut(2, 4)]), None);
    }

    #[test]
    fn a_word_whose_line_was_broken_since_is_not_widened() {
        let mut unplaced = item("bc");
        unplaced.start = None;
        let completion = Completion::new(1, 0, vec![unplaced], false);
        let accepted = completion.accept(&Rope::from_str("a\nb"), 3, plain);
        assert!(matches!(accepted, Some(Ok(_))));
    }

    /// The text after accepting `item` in `text`, with the cursor shown as
    /// `|`, where `|` in `text` is the cursor.
    fn accepted(text: &str, item: CompletionItem, start: usize) -> String {
        let cursor = text.find('|').unwrap();
        let rope = Rope::from_str(&text.replace('|', ""));
        let completion = Completion::new(1, start, vec![item], false);
        let accepted = completion.accept(&rope, cursor, plain).unwrap().unwrap();
        let mut after = accepted.edit.apply_to(&rope.to_string()).unwrap();
        after.insert(accepted.cursor, '|');
        after
    }

    #[test]
    fn edits_at_the_word_go_before_it_and_move_the_cursor_along() {
        let with = |label: &str, start: usize, more: Vec<(usize, usize, &str)>| {
            let mut item = item(label);
            item.start = Some(start);
            item.more = (more.into_iter())
                .map(|(start, end, text)| TextChange {
                    start,
                    end,
                    text: text.into(),
                })
                .collect();
            item
        };
        // `&` put before the word, as rust-analyzer's `&foo` does, with
        // some of it typed...
        assert_eq!(
            accepted("f(fo|)", with("foo", 2, vec![(2, 2, "&")]), 2),
            "f(&foo|)"
        );
        // ...and none of it.
        assert_eq!(
            accepted("f(|)", with("foo", 2, vec![(2, 2, "&")]), 2),
            "f(&foo|)"
        );
        // `.` made `->` before it, as clangd's fix does.
        assert_eq!(
            accepted("p.fi|", with("field", 2, vec![(1, 2, "->")]), 2),
            "p->field|"
        );
        // A snippet's cursor, past an import above.
        let mut call = with("call", 4, vec![(0, 0, "use c;\n")]);
        call.text = "call($1)".into();
        call.snippet = true;
        assert_eq!(accepted("x\n  ca|", call, 4), "use c;\nx\n  call(|)");
    }
}
