//! `/` and `?` in a chat's normal mode: search the conversation's text
//! forward or back, typed in the bar as an editor's search is, with its
//! history and Ctrl+R for a register (the desk has those), and `n` and
//! `N` search again, the same way or back, round the chat's ends.
//!
//! What's searched is the text the chat shows, block by block, as visual
//! mode selects it: messages, the agent's words and thinking without their
//! Markdown, and cards' lines. A match is within a block, never across
//! two. The pattern is Vim's, as an editor's search reads it (see
//! [`Pattern`]), its case as `ignorecase` and `smartcase` say.
//!
//! Folded work is searched too, as Vim's search opens folds: a match in a
//! turn's folded work opens it, and one in a tool's folded card opens the
//! card. Only turns the chat has loaded are searched: older ones come as
//! it's scrolled up to.
//!
//! The cursor goes to the match, and every match in view is lit up, as
//! Vim's `hlsearch` lights them, until Esc or `:nohlsearch`, and again at
//! the next search. Only blocks laid out and drawn are looked through for
//! them, no more than [`MOST_LIT`] in a block, and nothing once they're
//! not lit.
//!
//! A long chat isn't laid out, as laying out every turn would take
//! seconds. So each piece of a turn, its message, each of the agent's
//! words and each tool's card, is first looked through as the chat holds
//! it, words' blocks as their text and a card's lines as it would show them
//! open (no fonts needed). Only a piece that may hold the next match is
//! shown, opening its turn's work or its card if they're folded, and its
//! turn laid out, and searched as shown; what was opened for a piece that
//! turns out not to hold it is folded again.
//!
//! Each step of a search, to the next match, reads no more than
//! [`MOST_READ`] bytes of what the chat holds, each piece once, and lays
//! out no more than [`MOST_LAID`] turns or [`MOST_LAID_BYTES`] bytes of
//! what they hold, and says so if it stops short, so nothing stalls the
//! window. A count, as `40n`, takes as many steps, each with its own.
//!
//! A card shows no more than 1000 lines of its output: a match only in
//! those it leaves out is said to be there, rather than not found.

use clackatoo_protocol::{Item, Tool};

use super::card::Card;
use super::paint::{ITEMS, Position, after, blocks_text, control, folded_away};
use super::{ChatView, Input};
use crate::cmdline::{Keyed, Line};
use crate::editor::{Matching, Pattern};
use crate::fonts::Fonts;
use crate::input::KeyPress;
use crate::motion::Words;
use crate::options::Case;
use crate::vim::{WRAPPED_TO_BOTTOM, WRAPPED_TO_TOP};

/// The most bytes of what the chat holds one step of a search reads.
pub(super) const MOST_READ: usize = 64 << 20;

/// The most matches lit up in one block of text drawn.
const MOST_LIT: usize = 512;

/// The most turns one step of a search lays out to look in.
pub(super) const MOST_LAID: usize = 32;

/// The most bytes of what those turns hold one step of a search lays out:
/// laying out costs far more than reading.
pub(super) const MOST_LAID_BYTES: usize = 4 << 20;

/// A chat's search: the one being typed in the bar, and the last one, for
/// `n` and `N`, and whether each goes back.
#[derive(Debug, Default)]
pub(super) struct Searching {
    prompt: Option<Prompt>,
    last: Option<(String, bool)>,
    /// How searches treat case, as the options say.
    case: Case,
    /// The last search's pattern, while what it matches is lit up: from
    /// a search until Esc or `:nohlsearch`, as Vim's `hlsearch` lights it.
    lit: Option<Pattern>,
    /// Which search that is, counting from the first, for what was found
    /// of it to be kept against.
    generation: u64,
    /// What was last found lit up, and of what text drawn where.
    lit_kept: std::cell::RefCell<Option<(LitKey, Lit)>>,
}

/// The matches lit up: each one's block, by its place among those shown,
/// and its bytes.
type Lit = Vec<(usize, std::ops::Range<usize>)>;

/// What the matches lit up were found in: which search, how tall the view
/// is, and each block drawn, where, and how long its text is.
#[derive(Debug, PartialEq, Eq)]
struct LitKey {
    generation: u64,
    view: u32,
    blocks: Vec<LitBlock>,
}

/// A block of text drawn, as [`LitKey`] knows it.
#[derive(Debug, PartialEq, Eq)]
struct LitBlock {
    key: super::paint::TextKey,
    at: [u32; 2],
    len: usize,
}

#[cfg(test)]
thread_local! {
    /// How many times the matches lit up have been looked for, for tests.
    static LIT_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A search being typed.
#[derive(Debug)]
struct Prompt {
    line: Line,
    backward: bool,
    /// The count typed before `/` or `?`, as `3/x` finds the third.
    count: Option<usize>,
}

/// How far one step of a search has gone: what it spends, against
/// [`MOST_READ`], [`MOST_LAID`] and [`MOST_LAID_BYTES`].
struct Spent<'p> {
    pattern: &'p Pattern,
    read: usize,
    laid: usize,
    laid_bytes: usize,
    ran_out: bool,
    /// A card's output matched past what the card shows.
    hidden: bool,
}

impl<'p> Spent<'p> {
    fn new(pattern: &'p Pattern) -> Self {
        Self {
            pattern,
            read: 0,
            laid: 0,
            laid_bytes: 0,
            ran_out: false,
            hidden: false,
        }
    }

    /// Notes `bytes` more read. Returns whether that's within bounds.
    fn read(&mut self, bytes: usize) -> bool {
        self.read += bytes;
        self.ran_out |= self.read > MOST_READ;
        !self.ran_out
    }

    /// Notes a turn holding `bytes` laid out. Returns whether that's
    /// within bounds.
    /// The first a step lays out always is, however much it holds, so a
    /// turn too big for the bounds can still be searched.
    fn lay_out(&mut self, bytes: usize) -> bool {
        let first = self.laid == 0;
        self.laid += 1;
        self.laid_bytes += bytes;
        if !first {
            self.ran_out |= self.laid > MOST_LAID || self.laid_bytes > MOST_LAID_BYTES;
        }
        !self.ran_out
    }
}

/// A piece of a turn as a search looks through it: which, and what must be
/// opened for it to show, its turn's work or its card.
struct Looked {
    piece: usize,
    fold: bool,
    card: Option<String>,
}

impl ChatView {
    /// Stops lighting up what the last search matches, as Esc and
    /// `:nohlsearch` do, until the next search.
    pub(crate) fn stop_highlighting(&mut self) {
        self.search.lit = None;
    }

    /// What the last search matches in the text drawn, while it's lit up:
    /// each match's block, by its place among those shown, and its bytes.
    /// Only the lines of each block in view are looked through, no more
    /// than [`MOST_LIT`] matches in them, and nothing at all once it isn't
    /// lit. What was found is kept, so a frame that draws the same text
    /// in the same places doesn't look again.
    pub(crate) fn lit_matches(&self) -> Lit {
        let Some(pattern) = &self.search.lit else {
            return Vec::new();
        };
        let blocks: Vec<LitBlock> = (self.shown.iter())
            .map(|shown| LitBlock {
                key: shown.key,
                at: shown.at.map(f32::to_bits),
                len: self.text(shown.key).map_or(0, |text| text.text.len()),
            })
            .collect();
        let key = LitKey {
            generation: self.search.generation,
            view: self.view_height.to_bits(),
            blocks,
        };
        if let Some((kept, lit)) = &*self.search.lit_kept.borrow()
            && *kept == key
        {
            return lit.clone();
        }
        #[cfg(test)]
        LIT_SCANS.with(|scans| scans.set(scans.get() + 1));
        let mut lit = Vec::new();
        for (index, shown) in self.shown.iter().enumerate() {
            if control(shown.key.piece) {
                continue;
            }
            let Some(text) = self.text(shown.key) else {
                continue;
            };
            let Some(bytes) = self.bytes_in_view(shown, text.layout) else {
                continue;
            };
            let text = text.text;
            let mut at = bytes.start.min(text.len());
            for _ in 0..MOST_LIT {
                let Some(found) = (at <= text.len())
                    .then(|| pattern.find_at(text, at))
                    .flatten()
                    .filter(|found| found.start < bytes.end)
                else {
                    break;
                };
                if found.end > found.start {
                    lit.push((index, found.start..found.end));
                    at = found.end;
                } else if found.start < text.len() {
                    at = after(text, found.start);
                } else {
                    break;
                }
            }
        }
        *self.search.lit_kept.borrow_mut() = Some((key, lit.clone()));
        lit
    }

    /// The bytes of `shown`'s lines, laid out as `layout`, that are in
    /// view, if any are.
    fn bytes_in_view(
        &self,
        shown: &super::paint::Shown,
        layout: &parley::Layout<super::Brush>,
    ) -> Option<std::ops::Range<usize>> {
        let lines = layout.len();
        let bottom = |line: usize| {
            (layout.get(line)).map_or(f32::INFINITY, |line| line.metrics().block_max_coord)
        };
        let top = |line: usize| {
            (layout.get(line)).map_or(f32::INFINITY, |line| line.metrics().block_min_coord)
        };
        // The first line whose bottom is below the view's top, and the
        // first after it whose top is below the view's bottom.
        let first = first_line(lines, |line| shown.at[1] + bottom(line) > 0.0);
        let end = first_line(lines, |line| shown.at[1] + top(line) >= self.view_height);
        if first >= end {
            return None;
        }
        let start = layout.get(first)?.text_range().start;
        let stop = layout.get(end - 1)?.text_range().end;
        Some(start..stop)
    }

    /// Takes how searches treat case, as the options say.
    pub(crate) fn set_case(&mut self, case: Case) {
        self.search.case = case;
    }

    /// Whether a search is being typed, which takes every key.
    pub(crate) fn prompting(&self) -> bool {
        self.search.prompt.is_some()
    }

    /// The search being typed, for the desk to put a register in, or a
    /// search typed before.
    pub(crate) fn search_line(&mut self) -> Option<&mut Line> {
        self.search.prompt.as_mut().map(|prompt| &mut prompt.line)
    }

    /// What the bar shows of the search being typed, if one is: `/` or
    /// `?`, and what's typed.
    pub(crate) fn prompt_label(&self) -> Option<String> {
        let prompt = self.search.prompt.as_ref()?;
        let slash = if prompt.backward { '?' } else { '/' };
        Some(format!("{slash}{}", prompt.line.shown()))
    }

    /// The keys shown by Ctrl+? while a search is typed.
    pub(super) fn prompt_shortcuts(&self) -> Option<crate::shortcuts::Context> {
        let prompt = self.search.prompt.as_ref()?;
        Some(crate::shortcuts::prompt("Chat search", &prompt.line, true))
    }

    /// Forgets the search being typed, as leaving the chat does.
    pub(super) fn give_up_search(&mut self) {
        self.search.prompt = None;
    }

    /// `/`, or `?` if `backward`: starts typing a search in the bar, for
    /// the `count`th match.
    pub(super) fn start_search(&mut self, backward: bool, count: Option<usize>) {
        self.search.prompt = Some(Prompt {
            line: Line::default(),
            backward,
            count,
        });
    }

    /// A key typed into the search, edited as the command line is (see
    /// [`crate::cmdline`]); the desk has Up, Down and a register's name
    /// after Ctrl+R first. Enter searches, and with nothing typed, for the
    /// last search again, this way, as Vim's does. Returns whether a
    /// search was being typed, which takes every key.
    pub(super) fn prompt_key(&mut self, press: &KeyPress<'_>) -> bool {
        use crate::cmdline::controls::Control;

        let Some(prompt) = &mut self.search.prompt else {
            return false;
        };
        let control =
            (crate::vim::prompt_strokes(press).first()).map(|stroke| Control::read(*stroke).0);
        match control {
            Some(Control::Accept) => {
                let Some(mut prompt) = self.search.prompt.take() else {
                    return true;
                };
                let query = prompt.line.take();
                match &mut self.search.last {
                    _ if !query.is_empty() => self.search.last = Some((query, prompt.backward)),
                    Some((_, backward)) => *backward = prompt.backward,
                    None => return true,
                }
                self.search_again(false, prompt.count);
            }
            Some(Control::Cancel) => self.search.prompt = None,
            _ => {
                if prompt.line.key(press) == Keyed::Emptied {
                    self.search.prompt = None;
                }
            }
        }
        true
    }

    /// `n`, or `N` if `reverse`, `count` times: the last search again, the
    /// way it went, or back, from the cursor, once the fonts are at hand
    /// to lay out what it finds.
    pub(super) fn search_again(&mut self, reverse: bool, count: Option<usize>) {
        let Some((_, backward)) = self.search.last else {
            self.say("no search yet".to_owned());
            self.failed = true;
            return;
        };
        let Some(from) = self.cursor_or_default() else {
            return;
        };
        self.queued.push(Input::Search {
            from,
            backward: backward != reverse,
            count: count.unwrap_or(1),
        });
    }

    /// Searches for the last search from `from`, `count` times, backward
    /// if `backward`, round the chat's ends, and puts the cursor on what it
    /// finds. A count bigger than there are matches goes round as often
    /// as it says, but finds each match once.
    pub(super) fn searched(
        &mut self,
        fonts: &mut Fonts,
        from: Position,
        backward: bool,
        count: usize,
    ) {
        let Some((query, _)) = self.search.last.clone() else {
            return;
        };
        let pattern = match Pattern::new(&query, Matching::Typed(self.search.case), "", Words::NONE)
        {
            Ok(pattern) => pattern,
            Err(why) => {
                self.say(why);
                self.failed = true;
                return;
            }
        };
        self.search.lit = Some(pattern.clone());
        self.search.generation += 1;
        let (mut at, mut left, mut taken) = (from, count.max(1), 0);
        let (mut first, mut found, mut wrapped) = (None, None, false);
        let mut spent = Spent::new(&pattern);
        while left > 0 {
            // Each step has its own bounds.
            spent = Spent::new(&pattern);
            let Some((position, round)) = self.next_match(fonts, &mut spent, at, backward) else {
                break;
            };
            taken += 1;
            left -= 1;
            match first {
                None => first = Some(position),
                // Round to the first again: the rest of the count goes
                // round as many times, which ends where this many over.
                Some(first) if first == position => left %= taken - 1,
                Some(_) => {}
            }
            wrapped |= round;
            at = position;
            found = Some(position);
        }
        let Some(position) = found else {
            self.failed = true;
            let older = self.node.first_turn > 0;
            self.say(if spent.ran_out {
                "the chat is too long to search all of: search stopped".to_owned()
            } else if pattern.gave_up() {
                pattern.not_found(String::new())
            } else if spent.hidden {
                format!("{query} is only in output its card doesn't show")
            } else if older {
                format!("not found in the turns loaded: {query}")
            } else {
                format!("not found: {query}")
            });
            return;
        };
        self.go(position);
        self.goal = None;
        self.follow_selection();
        if wrapped {
            let said = if backward {
                WRAPPED_TO_BOTTOM
            } else {
                WRAPPED_TO_TOP
            };
            self.say(said.to_owned());
        }
    }

    /// The next match of `spent`'s pattern after `from`, or before it if
    /// `backward`, round the chat's ends to `from` itself, and whether it
    /// went round.
    fn next_match(
        &mut self,
        fonts: &mut Fonts,
        spent: &mut Spent<'_>,
        from: Position,
        backward: bool,
    ) -> Option<(Position, bool)> {
        let turns = self.node.turns.len();
        let start = from.text.turn.min(turns.checked_sub(1)?);
        for step in 0..=turns {
            let turn = if backward {
                (start + turns - step % turns) % turns
            } else {
                (start + step) % turns
            };
            let round = step == turns || if backward { turn > start } else { turn < start };
            let wanted = |position: Position| match (step, backward) {
                (0, false) => position > from,
                (0, true) => position < from,
                (_, false) if step == turns => position <= from,
                (_, true) if step == turns => position >= from,
                _ => true,
            };
            if let Some(found) = self.match_in_turn(fonts, spent, turn, backward, wanted) {
                return Some((found, round));
            }
            if spent.ran_out {
                return None;
            }
        }
        None
    }

    /// The first match in turn `turn` that's `wanted`, or the last if
    /// `backward`, as it shows: piece by piece, opening what's folded away
    /// of a piece that may hold one, and folding it again if it doesn't.
    fn match_in_turn(
        &mut self,
        fonts: &mut Fonts,
        spent: &mut Spent<'_>,
        turn: usize,
        backward: bool,
        wanted: impl Fn(Position) -> bool,
    ) -> Option<Position> {
        let activities = self.activities_of(turn);
        let mut pieces = self.pieces_of(turn, &activities);
        if backward {
            pieces.reverse();
        }
        for piece in pieces {
            // None of it is wanted: before the cursor, going on.
            let ends = [(0, 0), (usize::MAX, usize::MAX)].map(|(part, byte)| Position {
                text: super::paint::TextKey {
                    turn,
                    piece: piece.piece,
                    part,
                },
                byte,
            });
            if !ends.iter().any(|&end| wanted(end)) {
                continue;
            }
            let held = self.held_text(turn, piece.piece, &activities)?;
            if !spent.read(held.len()) {
                return None;
            }
            if spent.pattern.find_at(&held, 0).is_none() {
                continue;
            }
            if let Some(found) = self.match_in_piece(fonts, spent, turn, &piece, backward, &wanted)
            {
                return Some(found);
            }
            if spent.ran_out {
                return None;
            }
        }
        None
    }

    /// The first match in `piece` of turn `turn` that's `wanted`, or the
    /// last if `backward`, as it shows, once what it needs is opened and
    /// its turn laid out; what was opened is folded again if there's none.
    fn match_in_piece(
        &mut self,
        fonts: &mut Fonts,
        spent: &mut Spent<'_>,
        turn: usize,
        piece: &Looked,
        backward: bool,
        wanted: &impl Fn(Position) -> bool,
    ) -> Option<Position> {
        let folds_before = self.folds.get(&turn).copied();
        if piece.fold {
            self.set_fold(turn, true);
        }
        if let Some(id) = &piece.card {
            self.open_cards.insert(id.clone());
            self.restyled(turn);
        }
        let current = (self.cache.turns.get(turn)).is_some_and(super::paint::TurnSlot::current);
        let (found, any) = if current || spent.lay_out(self.turn_bytes(turn)) {
            self.lay_out_keeping_view(fonts, turn);
            self.exact_in(spent.pattern, turn, piece.piece, backward, wanted)
        } else {
            (None, false)
        };
        if found.is_some() {
            return found;
        }
        // A card's output matched, but not as it shows: past its lines.
        let tool = piece.piece >= ITEMS && self.card_at(turn, piece.piece);
        spent.hidden |= tool && !any && !spent.ran_out;
        if piece.fold || piece.card.is_some() {
            if piece.fold {
                match folds_before {
                    Some(open) => self.folds.insert(turn, open),
                    None => self.folds.remove(&turn),
                };
            }
            if let Some(id) = &piece.card {
                self.open_cards.remove(id);
            }
            // Not laid out again now: it keeps the height it was laid out
            // at, and the view the scroll that went with it, until it's
            // near the view, which the painter keeps where it is as it
            // lays it out again.
            self.restyled(turn);
        }
        None
    }

    /// The first match in `piece` of turn `turn`, laid out, that's
    /// `wanted`, or the last if `backward`; and whether it has any.
    fn exact_in(
        &self,
        pattern: &Pattern,
        turn: usize,
        piece: usize,
        backward: bool,
        wanted: &impl Fn(Position) -> bool,
    ) -> (Option<Position>, bool) {
        let (mut best, mut any) = (None, false);
        for key in self.turn_texts(turn) {
            if key.piece != piece || control(key.piece) {
                continue;
            }
            let Some(text) = self.text(key) else {
                continue;
            };
            let text = text.text;
            let mut at = 0;
            while let Some(found) = (at <= text.len())
                .then(|| pattern.find_at(text, at))
                .flatten()
            {
                any = true;
                let position = Position {
                    text: key,
                    byte: found.start,
                };
                if wanted(position) {
                    best = Some(position);
                    if !backward {
                        return (best, any);
                    }
                }
                at = if found.end > found.start {
                    found.end
                } else if found.start < text.len() {
                    after(text, found.start)
                } else {
                    break;
                };
            }
        }
        (best, any)
    }

    /// The pieces of turn `turn` a search looks through, in the order
    /// they show: its message, its items and the cards of `activities`,
    /// and what must open for each to show.
    fn pieces_of(&self, turn: usize, activities: &[Tool]) -> Vec<Looked> {
        let Some(at) = self.node.turns.get(turn) else {
            return Vec::new();
        };
        let shape = self.shape(turn, at);
        let closed = |tool: &Tool| (!self.open_cards.contains(&tool.id)).then(|| tool.id.clone());
        let mut pieces = vec![Looked {
            piece: super::paint::PROMPT,
            fold: false,
            card: None,
        }];
        for (index, item) in at.items.iter().enumerate() {
            pieces.push(Looked {
                piece: ITEMS + index,
                fold: folded_away(shape, index, item),
                card: match item {
                    Item::Tool(tool) => closed(tool),
                    _ => None,
                },
            });
        }
        for (index, tool) in activities.iter().enumerate() {
            pieces.push(Looked {
                piece: ITEMS + at.items.len() + index,
                fold: false,
                card: closed(tool),
            });
        }
        pieces
    }

    /// The text of `piece` of turn `turn` as the chat holds it, as it
    /// shows: a message as typed, words' blocks without their Markdown,
    /// and a card's lines, as open.
    fn held_text<'a>(
        &'a self,
        turn: usize,
        piece: usize,
        activities: &[Tool],
    ) -> Option<std::borrow::Cow<'a, str>> {
        use std::borrow::Cow;
        let at = self.node.turns.get(turn)?;
        if piece == super::paint::PROMPT {
            return Some(Cow::Borrowed(&at.prompt));
        }
        let index = piece.checked_sub(ITEMS)?;
        let owned = match at.items.get(index) {
            Some(Item::Words(text) | Item::Thought(text) | Item::Other(text)) => blocks_text(text),
            Some(Item::Tool(tool)) => card_text(tool),
            None => card_text(activities.get(index - at.items.len())?),
        };
        Some(Cow::Owned(owned))
    }

    /// Whether `piece` of turn `turn` is a tool's card.
    fn card_at(&self, turn: usize, piece: usize) -> bool {
        let Some(at) = self.node.turns.get(turn) else {
            return false;
        };
        let index = piece - ITEMS;
        index >= at.items.len() || matches!(at.items.get(index), Some(Item::Tool(_)))
    }

    /// About how much of turn `turn` shows, what laying it out costs: its
    /// message, the words its work's fold leaves, and each card's head,
    /// and for a card open, or a failed one, as many lines of its output
    /// or its change as it shows.
    fn turn_bytes(&self, turn: usize) -> usize {
        use super::paint::cards::{FOLDED, MOST};
        use clackatoo_protocol::{ToolContent, ToolStatus};
        let Some(at) = self.node.turns.get(turn) else {
            return 0;
        };
        let shape = self.shape(turn, at);
        let tool = |tool: &Tool| {
            let lines = if self.open_cards.contains(&tool.id) {
                MOST
            } else if tool.status == ToolStatus::Failed {
                FOLDED
            } else {
                0
            };
            let shown = (tool.content.iter())
                .map(|content| match content {
                    // A command's output shows its last lines.
                    ToolContent::Text(text)
                    | ToolContent::Output(text)
                    | ToolContent::Other(text) => lines_bytes(text, lines, true),
                    // A change, its first.
                    ToolContent::Diff(diff) => {
                        let old = diff.old.as_deref().unwrap_or_default();
                        lines_bytes(old, lines, false) + lines_bytes(&diff.new, lines, false)
                    }
                    ToolContent::Image(_) => 0,
                })
                .sum::<usize>();
            tool.title.len() + shown
        };
        let items = (at.items.iter().enumerate())
            .filter(|&(index, item)| !folded_away(shape, index, item))
            .map(|(_, item)| match item {
                Item::Words(text) | Item::Thought(text) | Item::Other(text) => text.len(),
                Item::Tool(found) => tool(found),
            })
            .sum::<usize>();
        let activities = self.activities_of(turn).iter().map(tool).sum::<usize>();
        at.prompt.len() + items + activities
    }

    /// The cards of the activities turn `turn` started, as it shows them,
    /// after its items.
    fn activities_of(&self, turn: usize) -> Vec<Tool> {
        (self.node.activities.iter())
            .filter(|activity| {
                activity.turn.checked_sub(self.node.first_turn) == u32::try_from(turn).ok()
            })
            .map(clackatoo_protocol::Activity::card)
            .collect()
    }
}

/// How many bytes the first `lines` lines of `text` take, or the last if
/// `last`, without reading further than they go.
fn lines_bytes(text: &str, lines: usize, last: bool) -> usize {
    if lines == 0 {
        return 0;
    }
    let found = if last {
        (text.trim_end_matches('\n').rmatch_indices('\n'))
            .nth(lines - 1)
            .map(|(at, _)| text.len() - at - 1)
    } else {
        text.match_indices('\n')
            .nth(lines - 1)
            .map(|(at, _)| at + 1)
    };
    found.unwrap_or(text.len())
}

/// The first of `lines` lines that `past` is true of, or `lines` if none
/// is, `past` being false of every line before one it's true of.
fn first_line(lines: usize, past: impl Fn(usize) -> bool) -> usize {
    let (mut low, mut high) = (0, lines);
    while low < high {
        let middle = low + (high - low) / 2;
        if past(middle) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low
}

/// `tool`'s card as a search reads it, as it shows open: its head, a
/// command's purpose, and its lines.
fn card_text(tool: &Tool) -> String {
    match Card::of(tool) {
        Some(Card::Command {
            command,
            purpose,
            output,
        }) => {
            let mut text = format!("$ {command}");
            for line in purpose.iter().chain(&output) {
                text.push('\n');
                text.push_str(line);
            }
            text
        }
        Some(card) => format!("{}\n{}", tool.title, card.text()),
        None => tool.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Item, Tool, ToolContent, ToolKind, ToolStatus};
    use winit::keyboard::NamedKey;

    use crate::chat::ChatView;
    use crate::chat::fixtures::{fonts, long_chat, paint, read, read_named, talk};
    use crate::chat::paint::{ITEMS, PROMPT, Position};
    use crate::fonts::Fonts;
    use crate::vim::{WRAPPED_TO_BOTTOM, WRAPPED_TO_TOP};

    fn reading(node: clackatoo_protocol::ChatNode) -> (ChatView, Fonts) {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), node);
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        (chat, fonts)
    }

    /// Types `query` after `/` or `?`, and Enter, and settles.
    fn search(chat: &mut ChatView, fonts: &mut Fonts, query: &str) {
        read(chat, query);
        read_named(chat, NamedKey::Enter);
        chat.settle(fonts);
    }

    fn at(chat: &ChatView) -> (usize, usize, usize) {
        let Position { text, byte } = chat.cursor.unwrap();
        (text.turn, text.piece, byte)
    }

    #[test]
    fn slash_searches_forward_and_n_goes_round() {
        let (mut chat, mut fonts) = reading(talk(&[
            ("find the cat", "a cat sat"),
            ("no", "another cat"),
        ]));
        read(&mut chat, "gg");
        // Typed in the bar, which shows it.
        read(&mut chat, "/ca");
        assert!(chat.prompting());
        assert_eq!(chat.prompt_label().as_deref(), Some("/ca▏"));
        search(&mut chat, &mut fonts, "t");
        assert!(!chat.prompting());
        assert_eq!(at(&chat), (0, PROMPT, "find the ".len()));
        read(&mut chat, "n");
        chat.settle(&mut fonts);
        assert_eq!(at(&chat), (0, ITEMS, 2));
        read(&mut chat, "n");
        chat.settle(&mut fonts);
        assert_eq!(at(&chat), (1, ITEMS, "another ".len()));
        // Round the end, saying so.
        read(&mut chat, "n");
        chat.settle(&mut fonts);
        assert_eq!(at(&chat), (0, PROMPT, "find the ".len()));
        assert_eq!(chat.label().as_deref(), Some(WRAPPED_TO_TOP));
        // `N` back, round the start.
        read(&mut chat, "N");
        chat.settle(&mut fonts);
        assert_eq!(at(&chat), (1, ITEMS, "another ".len()));
        assert_eq!(chat.label().as_deref(), Some(WRAPPED_TO_BOTTOM));
        // A count goes that many, round as often as it says.
        read(&mut chat, "gg");
        read(&mut chat, "8n");
        chat.settle(&mut fonts);
        assert_eq!(at(&chat), (0, ITEMS, 2));
    }

    #[test]
    fn question_mark_searches_back_and_enter_alone_searches_again() {
        let (mut chat, mut fonts) = reading(talk(&[("one cat", "two cats")]));
        read(&mut chat, "G$");
        search(&mut chat, &mut fonts, "?cat");
        assert_eq!(at(&chat), (0, ITEMS, "two ".len()));
        // `n` goes on back; `N` forward.
        read(&mut chat, "n");
        chat.settle(&mut fonts);
        assert_eq!(at(&chat), (0, PROMPT, "one ".len()));
        // Enter with nothing typed: the last search, the way typed.
        search(&mut chat, &mut fonts, "/");
        assert_eq!(at(&chat), (0, ITEMS, "two ".len()));
    }

    #[test]
    fn what_isnt_there_is_said_and_case_is_as_the_options_say() {
        let (mut chat, mut fonts) = reading(talk(&[("Hello", "world")]));
        read(&mut chat, "G");
        let before = chat.cursor;
        search(&mut chat, &mut fonts, "/nowhere");
        assert_eq!(chat.cursor, before);
        assert_eq!(chat.label().as_deref(), Some("not found: nowhere"));
        // Small letters match either case, by default.
        search(&mut chat, &mut fonts, "/hello");
        assert_eq!(at(&chat), (0, PROMPT, 0));
        // A capital matches exactly, with `smartcase`.
        read(&mut chat, "G");
        search(&mut chat, &mut fonts, "/World");
        assert_eq!(chat.label().as_deref(), Some("not found: World"));
        // Esc gives up what's typed.
        read(&mut chat, "/x");
        read_named(&mut chat, NamedKey::Escape);
        assert!(!chat.prompting());
        // `n` with no search says so.
        let (mut fresh, _) = reading(talk(&[("a", "b")]));
        read(&mut fresh, "n");
        assert_eq!(fresh.label().as_deref(), Some("no search yet"));
    }

    #[test]
    fn a_match_in_folded_work_opens_it_and_one_in_a_folded_card_opens_the_card() {
        let mut node = long_chat(3);
        let done = node.turns[0].end.clone();
        node.turns[2].end = done;
        let (mut chat, mut fonts) = reading(node);
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        let open = |chat: &ChatView, turn| chat.shape(turn, &chat.node.turns[turn]).open;
        assert!(!open(&chat, 0));
        // The agent's words in turn 0's work, folded away.
        search(&mut chat, &mut fonts, "/build it the way");
        assert!(open(&chat, 0), "opened to show the match");
        let (turn, piece, _) = at(&chat);
        assert_eq!(turn, 0);
        assert!(piece >= ITEMS, "{piece}");
        // A turn searched and not matched is folded again: turn 1's work
        // had none of this.
        search(&mut chat, &mut fonts, "/Turn 2: why");
        assert_eq!(at(&chat), (2, PROMPT, 0));
        assert!(!open(&chat, 1));
        // A tool's card.
        read(&mut chat, "gg");
        search(&mut chat, &mut fonts, "/workflows");
        let (turn, piece, _) = at(&chat);
        assert_eq!(turn, 0);
        let card = &chat.node.turns[0].items[piece - ITEMS];
        assert!(
            matches!(card, clackatoo_protocol::Item::Tool(tool) if tool.title.contains("ci.yml")),
            "{card:?}"
        );
    }

    #[test]
    fn a_search_in_a_long_chat_lays_out_only_what_may_match() {
        let mut node = long_chat(60);
        node.turns[59].end = node.turns[0].end.clone();
        node.turns[45].prompt.push_str(" Needle.");
        let (mut chat, mut fonts) = reading(node);
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        assert!(!chat.cache.turns[30].laid_out());
        search(&mut chat, &mut fonts, "/Needle");
        assert_eq!(at(&chat).0, 45);
        assert!(!chat.cache.turns[30].laid_out(), "not looked at whole");
    }

    /// A done turn of `items`, after a message.
    fn turn_of(items: Vec<Item>) -> clackatoo_protocol::ChatNode {
        let mut node = talk(&[("look", "")]);
        node.turns[0].items = items;
        node
    }

    fn command(id: &str, title: &str, content: Vec<ToolContent>) -> Item {
        Item::Tool(Tool {
            id: id.into(),
            title: title.into(),
            status: ToolStatus::Done,
            kind: ToolKind::Execute,
            content,
            locations: Vec::new(),
        })
    }

    #[test]
    fn only_the_card_that_may_hold_the_match_is_opened() {
        let mut node = long_chat(3);
        node.turns[2].end = node.turns[0].end.clone();
        let (mut chat, mut fonts) = reading(node);
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        search(&mut chat, &mut fonts, "/crate-150 ");
        let (turn, piece, _) = at(&chat);
        assert_eq!(turn, 0);
        assert_eq!(chat.open_cards.len(), 1, "{:?}", chat.open_cards);
        assert!(chat.open_cards.contains("0-build"));
        assert!(
            matches!(&chat.node.turns[0].items[piece - ITEMS], Item::Tool(tool) if tool.id == "0-build")
        );
        // Every line matches: `40n` goes on through them, never too long.
        search(&mut chat, &mut fonts, "/Compiling");
        read(&mut chat, "40n");
        chat.settle(&mut fonts);
        assert!(
            chat.label().is_none_or(|said| !said.contains("too long")),
            "{:?}",
            chat.label()
        );
        // A change's title, as its card's head shows it.
        read(&mut chat, "gg");
        search(&mut chat, &mut fonts, "/Edit src/lib");
        let (turn, piece, _) = at(&chat);
        assert_eq!(turn, 0);
        assert!(
            matches!(&chat.node.turns[0].items[piece - ITEMS], Item::Tool(tool) if tool.title == "Edit src/lib.rs")
        );
    }

    #[test]
    fn a_commands_purpose_and_a_long_title_are_found_as_its_card_shows_them() {
        let long = format!("echo {} needle", "word ".repeat(60));
        let node = turn_of(vec![
            command(
                "purpose",
                "make",
                vec![ToolContent::Text("Builds the docs".into())],
            ),
            command("long", &long, Vec::new()),
        ]);
        let (mut chat, mut fonts) = reading(node);
        read(&mut chat, "gg");
        search(&mut chat, &mut fonts, "/the docs");
        assert_eq!(at(&chat).1, ITEMS);
        // Past the summary a folded card shows: open, so the cursor's on
        // text that shows.
        search(&mut chat, &mut fonts, "/needle");
        let cursor = chat.cursor.unwrap();
        assert_eq!(cursor.text.piece, ITEMS + 1);
        assert!(chat.open_cards.contains("long"));
        paint(&mut chat, &mut fonts);
        let shown = chat.text(cursor.text).expect("laid out").text;
        assert_eq!(&shown[cursor.byte..cursor.byte + 6], "needle");
    }

    #[test]
    fn a_match_only_in_output_a_card_leaves_out_is_said_and_the_view_stays() {
        let lines: Vec<String> = (0..1500).map(|line| format!("line {line}")).collect();
        let output = format!("needle at the start\n{}\n", lines.join("\n"));
        let mut node = turn_of(vec![command(
            "long",
            "build",
            vec![ToolContent::Output(output)],
        )]);
        node.turns.extend(long_chat(4).turns);
        let last = node.turns.len() - 1;
        node.turns[last].end = node.turns[0].end.clone();
        let (mut chat, mut fonts) = reading(node);
        read(&mut chat, "G");
        paint(&mut chat, &mut fonts);
        // Up a little, so the view doesn't follow the chat's end.
        chat.scroll_lines(20.0);
        paint(&mut chat, &mut fonts);
        assert!(!chat.follow);
        let scroll = chat.scroll;
        search(&mut chat, &mut fonts, "/needle");
        assert_eq!(
            chat.label().as_deref(),
            Some("needle is only in output its card doesn't show")
        );
        assert!(chat.open_cards.is_empty(), "folded again");
        paint(&mut chat, &mut fonts);
        assert!(
            (chat.scroll - scroll).abs() < 0.5,
            "{} {scroll}",
            chat.scroll
        );
    }

    #[test]
    fn a_turn_holding_more_than_the_bounds_is_still_searched_as_it_shows() {
        // 5 MiB of output in a folded card, in a turn's folded work: what
        // shows is the message.
        let output = "x\n".repeat(5 << 19);
        let mut node = turn_of(vec![command(
            "big",
            "make",
            vec![ToolContent::Output(output)],
        )]);
        node.turns[0].prompt = "find the needle".into();
        let (mut chat, mut fonts) = reading(node);
        chat.cache.turns[0].stale = true;
        read(&mut chat, "G");
        search(&mut chat, &mut fonts, "/needle");
        assert_eq!(at(&chat), (0, PROMPT, "find the ".len()));
        assert!(chat.turn_bytes(0) < 100, "{}", chat.turn_bytes(0));
        // Open, in its turn's work opened, its last 1000 lines.
        chat.set_fold(0, true);
        chat.open_cards.insert("big".into());
        assert_eq!(
            chat.turn_bytes(0),
            "find the needle".len() + "make".len() + 2000
        );
    }

    #[test]
    fn the_first_layout_of_a_step_is_always_allowed() {
        let pattern = crate::editor::Pattern::new(
            "x",
            crate::editor::Matching::Exact,
            "",
            crate::motion::Words::NONE,
        )
        .unwrap();
        let mut spent = super::Spent::new(&pattern);
        assert!(spent.lay_out(super::MOST_LAID_BYTES * 2));
        assert!(!spent.lay_out(1));
        assert_eq!(super::lines_bytes("a\nbb\nccc\n", 2, false), 5);
        assert_eq!(super::lines_bytes("a\nbb\nccc\n", 2, true), 7);
        assert_eq!(super::lines_bytes("a\nbb", 9, true), 4);
        assert_eq!(super::lines_bytes("a\nbb", 0, true), 0);
    }

    #[test]
    fn every_match_in_view_is_lit_up_until_esc() {
        let lit = |chat: &mut ChatView, fonts: &mut Fonts| {
            let drawn = paint(chat, fonts);
            (drawn.fills.iter().chain(&drawn.over))
                .filter(|fill| fill.paint == crate::render::Paint::Found)
                .count()
        };
        let (mut chat, mut fonts) = reading(talk(&[
            ("find the cat", "a cat sat"),
            ("no", "another cat"),
        ]));
        assert_eq!(lit(&mut chat, &mut fonts), 0, "nothing searched");
        read(&mut chat, "gg");
        search(&mut chat, &mut fonts, "/cat");
        assert_eq!(lit(&mut chat, &mut fonts), 3);
        assert_eq!(chat.lit_matches().len(), 3);
        // Esc stops it, until the next search.
        read_named(&mut chat, NamedKey::Escape);
        assert_eq!(lit(&mut chat, &mut fonts), 0);
        read(&mut chat, "n");
        chat.settle(&mut fonts);
        assert_eq!(lit(&mut chat, &mut fonts), 3);
        chat.stop_highlighting();
        assert!(chat.lit_matches().is_empty());
    }

    #[test]
    fn the_matches_lit_are_those_in_view_of_a_long_block_found_once_a_frame() {
        let lines: Vec<String> = (0..3000).map(|line| format!("line {line}")).collect();
        let code = format!("```\n{}\n```", lines.join("\n"));
        let (mut chat, mut fonts) = reading(talk(&[("one", &code)]));
        search(&mut chat, &mut fonts, "/line");
        read(&mut chat, "GN");
        chat.settle(&mut fonts);
        paint(&mut chat, &mut fonts);
        let cursor = chat.cursor.unwrap();
        let lit = chat.lit_matches();
        assert!(!lit.is_empty());
        assert!(
            lit.iter()
                .any(|(index, range)| chat.shown[*index].key == cursor.text
                    && range.start == cursor.byte),
            "the match at the cursor, near the end, is lit: {cursor:?}"
        );
        // Only what's in view: far fewer than the block's 3000.
        assert!(lit.len() < 200, "{}", lit.len());
        // A frame drawing the same doesn't look again.
        let scans = || super::LIT_SCANS.with(std::cell::Cell::get);
        let before = scans();
        paint(&mut chat, &mut fonts);
        paint(&mut chat, &mut fonts);
        assert_eq!(scans(), before, "kept");
        chat.scroll_lines(5.0);
        paint(&mut chat, &mut fonts);
        assert_eq!(scans(), before + 1, "looked again once it scrolled");
    }

    #[test]
    fn a_count_before_slash_finds_that_match() {
        let (mut chat, mut fonts) = reading(talk(&[("cat", "cat cat cat")]));
        read(&mut chat, "gg");
        search(&mut chat, &mut fonts, "3/cat");
        assert_eq!(at(&chat), (0, ITEMS, "cat cat ".len()));
    }
}
