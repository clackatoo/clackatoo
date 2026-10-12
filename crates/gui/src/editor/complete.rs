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
//! Snippets are made plain text: each place to type becomes the text it
//! starts with, and the cursor goes to the first. There's no jumping from
//! place to place.

use std::collections::HashMap;

use clackatoo_protocol::{CompletionItem, Edit};
use clackatoo_text::{Bias, Rope};
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

/// Whether `ch` is part of a word being completed: letters, digits and `_`,
/// as `w` reads them.
pub(crate) fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

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
        }
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
    /// replaced with its text, as a snippet's made plain, and its edits
    /// elsewhere are made with it, but any that would overlap that. `None`
    /// if none is picked; an error, saying why, if its edits make no sense.
    pub(crate) fn accept(&self, text: &Rope, cursor: usize) -> Option<Result<Accepted, String>> {
        let item = self.item(self.picked()?)?;
        // Where the server said, if it's not past the cursor or on a line
        // before, or else where the word starts.
        let line_start = text.line_to_byte(text.byte_to_line(cursor.min(text.len_bytes())));
        let start = (item.start.map(|start| self.now(start)))
            .filter(|&start| start <= cursor && start >= line_start)
            .unwrap_or(self.start.min(cursor));
        let (put, offset) = if item.snippet {
            expand(&item.text)
        } else {
            (item.text.clone(), item.text.len())
        };
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
        Some(Ok(Accepted {
            cursor: start.saturating_add_signed(moved) + offset,
            edit,
            text: put,
            offset,
            replaced,
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
                Ink::Accent
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

/// `snippet`, as LSP writes them, made plain text: each place to type, as
/// `$1`, `${2:name}` or `${3|a,b|}`, becomes the text it starts with, and
/// variables the text they default to. Returns it, and the byte the cursor
/// goes to: the first place to type, or else the last, `$0`, or else the
/// end.
pub(crate) fn expand(snippet: &str) -> (String, usize) {
    let chars: Vec<char> = snippet.chars().collect();
    let mut reading = Snippet {
        chars: &chars,
        at: 0,
        text: String::new(),
        stops: Vec::new(),
    };
    reading.read(false);
    let first = (reading.stops.iter())
        .filter(|(number, _)| *number > 0)
        .min_by_key(|(number, _)| *number)
        .or_else(|| reading.stops.iter().find(|(number, _)| *number == 0));
    let cursor = first.map_or(reading.text.len(), |&(_, at)| at);
    (reading.text, cursor)
}

/// A snippet being read.
struct Snippet<'a> {
    chars: &'a [char],
    at: usize,
    /// What it makes, so far.
    text: String,
    /// Each place to type, by its number, and where it starts in `text`.
    stops: Vec<(u32, usize)>,
}

impl Snippet<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    /// Reads to the end, or if `nested`, to the `}` ending what it's in.
    fn read(&mut self, nested: bool) {
        while let Some(ch) = self.peek() {
            self.at += 1;
            match ch {
                '\\' => match self.peek() {
                    Some(next @ ('$' | '}' | '\\' | ',' | '|')) => {
                        self.at += 1;
                        self.text.push(next);
                    }
                    _ => self.text.push('\\'),
                },
                '}' if nested => return,
                '$' => self.dollar(),
                _ => self.text.push(ch),
            }
        }
    }

    /// Reads what follows a `$`.
    fn dollar(&mut self) {
        match self.peek() {
            Some(ch) if ch.is_ascii_digit() => {
                let number = self.number();
                self.stops.push((number, self.text.len()));
            }
            Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {
                // A variable, which nothing here knows: nothing.
                while self
                    .peek()
                    .is_some_and(|ch| ch == '_' || ch.is_ascii_alphanumeric())
                {
                    self.at += 1;
                }
            }
            Some('{') => {
                self.at += 1;
                if self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                    let number = self.number();
                    self.stops.push((number, self.text.len()));
                } else {
                    while self
                        .peek()
                        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphanumeric())
                    {
                        self.at += 1;
                    }
                }
                match self.peek() {
                    // Its text, which may hold more.
                    Some(':') => {
                        self.at += 1;
                        self.read(true);
                    }
                    Some('|') => {
                        self.at += 1;
                        self.choices();
                    }
                    Some('}') => self.at += 1,
                    // Not a snippet's: skipped to its end.
                    _ => {
                        let mut text = std::mem::take(&mut self.text);
                        self.read(true);
                        std::mem::swap(&mut text, &mut self.text);
                    }
                }
            }
            _ => self.text.push('$'),
        }
    }

    /// Reads digits.
    fn number(&mut self) -> u32 {
        let mut number: u32 = 0;
        while let Some(digit) = self.peek().and_then(|ch| ch.to_digit(10)) {
            number = number.saturating_mul(10).saturating_add(digit);
            self.at += 1;
        }
        number
    }

    /// Reads a choice's options, `a,b|}`, keeping the first.
    fn choices(&mut self) {
        let mut first = true;
        while let Some(ch) = self.peek() {
            self.at += 1;
            match ch {
                '\\' => {
                    if let Some(next) = self.peek() {
                        self.at += 1;
                        if first {
                            self.text.push(next);
                        }
                    }
                }
                ',' => first = false,
                '|' if self.peek() == Some('}') => {
                    self.at += 1;
                    return;
                }
                _ if first => self.text.push(ch),
                _ => {}
            }
        }
    }
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
            .filter(|run| run.ink == Ink::Accent)
            .map(|run| run.text.as_str())
            .collect();
        assert_eq!(lit, ["p", "l"]);
        assert!(line.picked);
        assert_eq!(line.text(), "var     print_line");
    }

    #[test]
    fn snippets_are_made_plain_text() {
        assert_eq!(expand("call(${1:a}, ${2:b})$0"), ("call(a, b)".into(), 5));
        assert_eq!(expand("f($0)"), ("f()".into(), 2));
        assert_eq!(expand("f($2, $1)"), ("f(, )".into(), 4));
        assert_eq!(expand("plain"), ("plain".into(), 5));
        // Nested, chosen, escaped, and variables.
        assert_eq!(
            expand("${1:outer ${2:inner}} ${3|one,two|} \\$x \\} ${TM_FILENAME} $NAME"),
            ("outer inner one $x }  ".into(), 0)
        );
        assert_eq!(expand("${VAR:default}!"), ("default!".into(), 8));
        // Broken ones still make text.
        assert_eq!(expand("a$"), ("a$".into(), 2));
        assert_eq!(expand("${1:open"), ("open".into(), 0));
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
        let accepted = completion.accept(&text, cursor).unwrap().unwrap();
        let after = accepted.edit.apply_to(&text.to_string()).unwrap();
        assert_eq!(after, "use std::io;\nfn main() {\n    println!(fmt)\n}\n");
        assert_eq!(&after[accepted.cursor..], "fmt)\n}\n");
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
        let accepted = completion.accept(&text, cursor + 4).unwrap().unwrap();
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

    /// The text after accepting `item` in `text`, with the cursor shown as
    /// `|`, where `|` in `text` is the cursor.
    fn accepted(text: &str, item: CompletionItem, start: usize) -> String {
        let cursor = text.find('|').unwrap();
        let rope = Rope::from_str(&text.replace('|', ""));
        let completion = Completion::new(1, start, vec![item], false);
        let accepted = completion.accept(&rope, cursor).unwrap().unwrap();
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
