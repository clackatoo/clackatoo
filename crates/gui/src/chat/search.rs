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
//! The cursor goes to the match; matches aren't lit up, as the chat's
//! painter has no pass for them.
//!
//! A long chat isn't laid out, as laying out every turn would take
//! seconds. So each turn is first looked through as the chat has it, its
//! words' blocks as their text (no fonts needed), and only a turn that may
//! match is laid out and searched as shown. One search goes through no
//! more than [`MOST_SEARCHED`] bytes and lays out no more than
//! [`MOST_LAID`] turns, and says so if it stops short, so nothing stalls
//! the window.

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

/// The most bytes of a chat's text one search goes through, its laid out
/// blocks and the turns it looks through as the chat has them.
pub(super) const MOST_SEARCHED: usize = 64 << 20;

/// The most turns one search lays out to look in.
pub(super) const MOST_LAID: usize = 32;

/// A chat's search: the one being typed in the bar, and the last one, for
/// `n` and `N`, and whether each goes back.
#[derive(Debug, Default)]
pub(super) struct Searching {
    prompt: Option<Prompt>,
    last: Option<(String, bool)>,
    /// How searches treat case, as the options say.
    case: Case,
}

/// A search being typed.
#[derive(Debug)]
struct Prompt {
    line: Line,
    backward: bool,
}

/// How far one search has gone: what it spends, against [`MOST_SEARCHED`]
/// and [`MOST_LAID`].
struct Spent<'p> {
    pattern: &'p Pattern,
    searched: usize,
    laid: usize,
    ran_out: bool,
}

impl Spent<'_> {
    /// Notes `bytes` more searched. Returns whether that's within bounds.
    fn search(&mut self, bytes: usize) -> bool {
        self.searched += bytes;
        if self.searched > MOST_SEARCHED {
            self.ran_out = true;
        }
        !self.ran_out
    }
}

impl ChatView {
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

    /// `/`, or `?` if `backward`: starts typing a search in the bar.
    pub(super) fn start_search(&mut self, backward: bool) {
        self.search.prompt = Some(Prompt {
            line: Line::default(),
            backward,
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
                self.search_again(false, None);
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
                return;
            }
        };
        let mut spent = Spent {
            pattern: &pattern,
            searched: 0,
            laid: 0,
            ran_out: false,
        };
        let (mut at, mut left, mut taken) = (from, count.max(1), 0);
        let (mut first, mut found, mut wrapped) = (None, None, false);
        while left > 0 {
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
            let older = self.node.first_turn > 0;
            self.say(if spent.ran_out {
                "the chat is too long to search all of: search stopped".to_owned()
            } else if pattern.gave_up() {
                pattern.not_found(String::new())
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
    /// `backward`, as it shows: what's folded away that matches is opened
    /// to look, and folded again unless that's where the match is.
    fn match_in_turn(
        &mut self,
        fonts: &mut Fonts,
        spent: &mut Spent<'_>,
        turn: usize,
        backward: bool,
        wanted: impl Fn(Position) -> bool,
    ) -> Option<Position> {
        let pattern = spent.pattern;
        let (might, fold, cards) = self.might_match(spent, turn)?;
        if !might {
            return None;
        }
        let folds_before = self.folds.get(&turn).copied();
        let shape_before = self.shape(turn, &self.node.turns[turn]);
        if fold {
            self.set_fold(turn, true);
        }
        for id in &cards {
            self.open_cards.insert(id.clone());
        }
        if !cards.is_empty() {
            self.restyled(turn);
        }
        let current = (self.cache.turns.get(turn)).is_some_and(super::paint::TurnSlot::current);
        if !current {
            if spent.laid >= MOST_LAID {
                spent.ran_out = true;
            } else {
                spent.laid += 1;
                self.lay_out_keeping_view(fonts, turn);
            }
        }
        let mut best = None;
        if !spent.ran_out {
            'texts: for key in self.turn_texts(turn) {
                if control(key.piece) {
                    continue;
                }
                let Some(text) = self.text(key) else {
                    continue;
                };
                let text = text.text;
                if !spent.search(text.len()) {
                    break;
                }
                let mut at = 0;
                while let Some(found) = (at <= text.len())
                    .then(|| pattern.find_at(text, at))
                    .flatten()
                {
                    let position = Position {
                        text: key,
                        byte: found.start,
                    };
                    if wanted(position) {
                        best = Some(position);
                        if !backward {
                            break 'texts;
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
        }
        // What was opened to look, and doesn't hold the match, is folded
        // again.
        let holds = |piece: usize| best.is_some_and(|best: Position| best.text.piece == piece);
        let in_folded = best.is_some_and(|best| {
            let index = best.text.piece.wrapping_sub(ITEMS);
            let items = &self.node.turns[turn].items;
            (items.get(index)).is_some_and(|item| folded_away(shape_before, index, item))
        });
        if fold && !in_folded {
            match folds_before {
                Some(open) => self.folds.insert(turn, open),
                None => self.folds.remove(&turn),
            };
            self.restyled(turn);
        }
        for id in cards {
            let piece = self.card_piece(turn, &id);
            // A card's head shows folded too.
            let in_head = best.is_some_and(|best| best.text.part == 0);
            if !piece.is_some_and(holds) || in_head {
                self.open_cards.remove(&id);
                self.restyled(turn);
            }
        }
        best
    }

    /// Whether turn `turn` may match `spent`'s pattern, as the chat has
    /// it, and if it may, whether its work must be opened, and which
    /// tools' cards, for what may match to show. `None` if that's more
    /// than the search may spend.
    fn might_match(&self, spent: &mut Spent<'_>, turn: usize) -> Option<(bool, bool, Vec<String>)> {
        let pattern = spent.pattern;
        let at = self.node.turns.get(turn)?;
        let shape = self.shape(turn, at);
        let matches = |spent: &mut Spent<'_>, text: &str| {
            spent
                .search(text.len())
                .then(|| pattern.find_at(text, 0).is_some())
        };
        let mut might = matches(spent, &at.prompt)?;
        let (mut fold, mut cards) = (false, Vec::new());
        let tool = |spent: &mut Spent<'_>, tool: &Tool, cards: &mut Vec<String>| {
            let text = Card::of(tool).map_or_else(|| tool.title.clone(), |card| card.text());
            let found = matches(spent, &text)?;
            if found && !self.open_cards.contains(&tool.id) {
                cards.push(tool.id.clone());
            }
            Some(found)
        };
        for (index, item) in at.items.iter().enumerate() {
            let found = match item {
                Item::Words(text) | Item::Thought(text) | Item::Other(text) => {
                    matches(spent, &blocks_text(text))?
                }
                Item::Tool(found) => tool(spent, found, &mut cards)?,
            };
            if found {
                might = true;
                fold |= folded_away(shape, index, item);
            }
        }
        for activity in self.activities_of(turn) {
            might |= tool(spent, &activity, &mut cards)?;
        }
        Some((might, fold, cards))
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

    /// Which piece of turn `turn` the card of the tool `id` is.
    fn card_piece(&self, turn: usize, id: &str) -> Option<usize> {
        let items = &self.node.turns.get(turn)?.items;
        let item =
            (items.iter()).position(|item| matches!(item, Item::Tool(tool) if tool.id == id));
        let activity = || {
            (self.activities_of(turn).iter())
                .position(|tool| tool.id == id)
                .map(|at| items.len() + at)
        };
        item.or_else(activity).map(|index| ITEMS + index)
    }
}

#[cfg(test)]
mod tests {
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
}
