//! A chat block as the shared motion engine sees it. Rows are laid out
//! lazily and retained for the current block; no rope or whole-chat copy is needed.
use std::cell::{OnceCell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use crate::editor::Laid;
use crate::motion::{Cells, Lines, Point, Words};

pub(super) struct Block {
    text: String,
    rows: Vec<(Range<usize>, OnceCell<Laid>)>,
    words: Words,
}

impl Block {
    pub(super) fn new(text: &str, words: Words) -> Self {
        let mut start = 0;
        let mut ranges = Vec::new();
        for line in text.split_inclusive('\n') {
            let end = start + line.trim_end_matches('\n').len();
            ranges.push(start..end);
            start += line.len();
        }
        if ranges.is_empty() {
            ranges.push(0..0);
        }
        Self::ranges(text, words, ranges)
    }

    pub(super) fn ranges(text: &str, words: Words, mut ranges: Vec<Range<usize>>) -> Self {
        if ranges.is_empty() {
            ranges.push(0..0);
        }
        Self {
            text: text.to_owned(),
            words,
            rows: ranges.into_iter().map(|r| (r, OnceCell::new())).collect(),
        }
    }

    /// Empty boundary rows let the engine report crossing a block without
    /// inventing another definition of a word in the chat.
    pub(super) fn bounded(text: &str, words: Words) -> Self {
        let mut block = Self::new(text, words);
        block.rows.insert(0, (0..0, OnceCell::new()));
        block.rows.push((text.len()..text.len(), OnceCell::new()));
        block
    }

    pub(super) fn point(&self, byte: usize) -> Point {
        let byte = byte.min(self.text.len());
        let line = self
            .rows
            .partition_point(|(r, _)| r.start <= byte)
            .saturating_sub(1);
        let start = self.rows[line].0.start;
        Point {
            line: line as u64,
            col: self
                .row(line as u64)
                .unwrap()
                .col_of(byte.saturating_sub(start)),
        }
    }

    pub(super) fn byte(&self, at: Point) -> usize {
        let line = usize::try_from(at.line)
            .unwrap_or(usize::MAX)
            .min(self.rows.len() - 1);
        self.rows[line].0.start + self.row(line as u64).unwrap().byte_at(at.col)
    }

    pub(super) fn char_end(&self, at: Point) -> usize {
        let line = usize::try_from(at.line)
            .unwrap_or(usize::MAX)
            .min(self.rows.len() - 1);
        let laid = self.row(line as u64).unwrap();
        self.rows[line].0.start + laid.char_end(laid.byte_at(at.col))
    }

    pub(super) fn range(&self, line: u64) -> Range<usize> {
        self.rows[usize::try_from(line)
            .unwrap_or(usize::MAX)
            .min(self.rows.len() - 1)]
        .0
        .clone()
    }
}

impl Lines for Block {
    type Line = Laid;
    type Row<'a>
        = &'a Laid
    where
        Self: 'a;
    fn row(&self, line: u64) -> Option<&Laid> {
        let (range, laid) = self.rows.get(usize::try_from(line).ok()?)?;
        Some(laid.get_or_init(|| Laid::new(self.text[range.clone()].to_owned(), 8)))
    }
    fn bounds(&self) -> (u64, u64) {
        (0, self.rows.len().saturating_sub(1) as u64)
    }
    fn empty(&self, line: u64) -> bool {
        self.rows
            .get(usize::try_from(line).unwrap_or(usize::MAX))
            .is_none_or(|(r, _)| r.is_empty())
    }
    fn empty_in(&self, line: u64, _row: Option<&Laid>) -> bool {
        self.empty(line)
    }
    fn last_cell_in(&self, _line: u64, row: Option<&Laid>) -> usize {
        row.and_then(|laid| Some(laid.extent(laid.cells().checked_sub(1)?).0))
            .unwrap_or(0)
    }
    fn words(&self) -> Words {
        self.words
    }
}

use super::ChatView;
use super::paint::{How, Picked, Position, TextKey, control};
use crate::motion::Motion;
use crate::objects::{Object, Selected};
use crate::vim::{Selection, ViewAt};

/// One current block for each addressing mode: hard lines, visual rows,
/// and hard lines with boundary sentinels. Invalidated by layout or word
/// policy changes, and bounded independently of conversation length.
#[derive(Default)]
pub(super) struct MotionCache(RefCell<[Option<Memo>; 3]>);

struct Memo {
    key: TextKey,
    layout: u64,
    block: Rc<Block>,
}

impl ChatView {
    pub(super) fn motion_block(
        &self,
        key: TextKey,
        visual: bool,
        bounded: bool,
    ) -> Option<Rc<Block>> {
        let text = self.text(key)?;
        let words = self.block_words(key);
        let layout = self.laid_at(key.turn);
        let index = if bounded { 2 } else { usize::from(visual) };
        let mut cache = self.motion_cache.0.borrow_mut();
        if let Some(memo) = &cache[index]
            && memo.key == key
            && memo.layout == layout
            && memo.block.words == words
        {
            return Some(Rc::clone(&memo.block));
        }
        let block = if visual {
            Block::ranges(
                text.text,
                words,
                (0..text.layout.len())
                    .filter_map(|line| {
                        super::paint::line_middle(text.layout, line).map(|(_, range)| {
                            let end = range.end.min(text.text.len());
                            range.start
                                ..range.start
                                    + text.text[range.start..end]
                                        .trim_end_matches(['\n', '\r'])
                                        .len()
                        })
                    })
                    .collect(),
            )
        } else if bounded {
            Block::bounded(text.text, words)
        } else {
            Block::new(text.text, words)
        };
        let block = Rc::new(block);
        cache[index] = Some(Memo {
            key,
            layout,
            block: Rc::clone(&block),
        });
        Some(block)
    }

    pub(super) fn block_words(&self, key: TextKey) -> Words {
        if let Some(text) = self.text(key)
            && let Some(crate::markdown::BlockKind::Code(Some(language))) = text.kind
        {
            return self.colours.words(language);
        }
        Words::NONE
    }

    pub(super) fn reselect(&mut self) {
        let Some(saved) = self.last_visual else {
            self.failed = true;
            return;
        };
        let needs_layout = [saved.anchor, saved.head].into_iter().any(|at| {
            at.text.turn < self.node.turns.len()
                && self
                    .cache
                    .turns
                    .get(at.text.turn)
                    .is_none_or(|slot| !slot.laid_out())
        });
        if needs_layout {
            self.queued.push(super::Input::Reselect);
            return;
        }
        let valid = [saved.anchor, saved.head].into_iter().all(|at| {
            self.turn_texts(at.text.turn).contains(&at.text)
                && self.text(at.text).is_some_and(|text| {
                    at.byte <= text.text.len() && text.text.is_char_boundary(at.byte)
                })
        });
        if !valid {
            self.last_visual = None;
            self.failed = true;
            return;
        }
        if self.visual().is_some() {
            self.last_visual = self.picked;
        }
        self.picked = Some(saved);
        self.go(saved.head);
        self.goal = None;
    }

    pub(super) fn select_object(&mut self, object: Object, inner: bool, count: usize) {
        let Some(at) = self.cursor_or_default() else {
            return;
        };
        let Some(picked) = self.picked else {
            return;
        };
        if matches!(object, Object::Message | Object::CodeBlock) {
            let keys = self.turn_texts(at.text.turn);
            let keys: Vec<_> = keys
                .into_iter()
                .filter(|key| {
                    !control(key.piece)
                        && (object == Object::Message && (!inner || key.piece == at.text.piece)
                            || object == Object::CodeBlock && *key == at.text)
                })
                .collect();
            if object == Object::CodeBlock
                && !self.text(at.text).is_some_and(|text| {
                    matches!(text.kind, Some(crate::markdown::BlockKind::Code(_)))
                })
            {
                self.failed = true;
                return;
            }
            let Some((&first, &last)) = keys.first().zip(keys.last()) else {
                self.failed = true;
                return;
            };
            let byte = self
                .text(last)
                .map_or(0, |text| super::paint::before(text.text, text.text.len()));
            let head = Position { text: last, byte };
            self.picked = Some(Picked {
                anchor: Position {
                    text: first,
                    byte: 0,
                },
                head,
                how: How::Visual(Selection::Lines),
            });
            self.go(head);
            self.goal = None;
            return;
        }
        if picked.anchor.text != at.text {
            self.failed = true;
            return;
        }
        let Some(block) = self.motion_block(at.text, false, false) else {
            return;
        };
        let kind = self.visual().unwrap_or(Selection::Characters);
        let selected = Selected {
            anchor: block.point(picked.anchor.byte),
            cursor: block.point(at.byte),
            lines: kind == Selection::Lines,
        };
        let result = object.select(&*block, selected, inner, count);
        let failed = result.is_err();
        let (Ok(selected) | Err(Some(selected))) = result else {
            self.failed = true;
            return;
        };
        let anchor = Position {
            byte: block.byte(selected.anchor),
            ..at
        };
        let head = Position {
            byte: block.byte(selected.cursor),
            ..at
        };
        let kind = object.selection(kind, selected.lines, failed);
        self.picked = Some(Picked {
            anchor,
            head,
            how: How::Visual(kind),
        });
        self.go(head);
        self.goal = None;
        self.failed |= failed;
    }

    /// Visible proportional rows, in reading order. Only fully visible
    /// rows count, unless a short viewport has none of those.
    fn visible_rows(&self) -> Vec<(Position, f32)> {
        let mut rows = Vec::new();
        let first = self
            .cache
            .bounds
            .partition_point(|bound| bound[1] <= self.scroll);
        for turn in first..self.cache.bounds.len() {
            let bound = self.cache.bounds[turn];
            if bound[0] >= self.scroll + self.view_height {
                break;
            }
            for key in self.turn_texts(turn) {
                if control(key.piece) {
                    continue;
                }
                let Some(text) = self.text(key) else {
                    continue;
                };
                for line in 0..text.layout.len() {
                    let Some((middle, range)) = super::paint::line_middle(text.layout, line) else {
                        continue;
                    };
                    let y = bound[0] + text.at[1] + middle - self.scroll;
                    if (0.0..self.view_height).contains(&y) {
                        rows.push((
                            Position {
                                text: key,
                                byte: range.start,
                            },
                            y,
                        ));
                    }
                }
            }
        }
        rows
    }

    pub(super) fn view_motion(
        &mut self,
        motion: Motion,
        count: Option<usize>,
        taking: bool,
    ) -> bool {
        let rows = self.visible_rows();
        if rows.is_empty() {
            return false;
        }
        let last = rows.len() - 1;
        let margin = if taking {
            0
        } else {
            (self.scroll_margin as usize).min(last / 2)
        };
        let n = count.unwrap_or(1).saturating_sub(1);
        let index = match motion {
            Motion::ViewTop => n.max(if self.scroll <= 0.0 { 0 } else { margin }).min(last),
            Motion::ViewBottom => last.saturating_sub(n.max(if self.scroll >= self.max_scroll {
                0
            } else {
                margin
            })),
            _ => last / 2,
        };
        let mut at = rows[index].0;
        if let Some(text) = self.text(at.text) {
            let line = super::paint::line_of(text.layout, at.byte);
            if let Some((middle, range)) = super::paint::line_middle(text.layout, line) {
                if self.start_of_line {
                    let row = &text.text[range.start..range.end.min(text.text.len())];
                    at.byte = range.start + row.len() - row.trim_start().len();
                } else if let Some(goal) = self.goal.or_else(|| {
                    self.cursor.and_then(|cursor| {
                        self.text(cursor.text)
                            .map(|text| text.at[0] + super::paint::x_of(text.layout, cursor.byte))
                    })
                }) {
                    at.byte =
                        parley::Cursor::from_point(text.layout, goal - text.at[0], middle).index();
                    at.byte = at.byte.min(
                        super::paint::before(text.text, range.end.min(text.text.len()))
                            .max(range.start),
                    );
                }
            }
        }
        self.go(at);
        true
    }

    pub(super) fn place_view(&mut self, at: ViewAt) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let Some(bound) = self.cache.bounds.get(cursor.text.turn) else {
            return;
        };
        let Some(text) = self.text(cursor.text) else {
            return;
        };
        let line = super::paint::line_of(text.layout, cursor.byte);
        let Some((middle, _)) = super::paint::line_middle(text.layout, line) else {
            return;
        };
        let y = bound[0] + text.at[1] + middle - self.scroll;
        let target = match at {
            ViewAt::Top => self.line_height / 2.0,
            ViewAt::Middle => self.view_height / 2.0,
            ViewAt::Bottom => self.view_height - self.line_height / 2.0,
        };
        self.place_after_layout = None;
        self.scroll_by(y - target);
        self.reveal = false;
        self.keep_cursor = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::fixtures::{chars, fonts, paint, read, read_named, talk, yanked};
    use winit::keyboard::NamedKey;

    fn reading(answer: &str) -> (ChatView, crate::fonts::Fonts) {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), talk(&[("prompt", answer)]));
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "gg}");
        (chat, fonts)
    }

    #[test]
    fn new_motions_and_yanks_use_shared_word_bracket_and_sentence_rules() {
        let (mut chat, mut fonts) =
            reading("```\n  alpha 中文beta (x)  \nSecond sentence. Next one.\n```");
        read(&mut chat, "^w");
        assert_eq!(chat.cursor.unwrap().byte, "  alpha ".len());
        read(&mut chat, "w");
        assert_eq!(chat.cursor.unwrap().byte, "  alpha 中文".len());
        read(&mut chat, "ge");
        assert_eq!(chat.cursor.unwrap().byte, "  alpha 中".len());
        read(&mut chat, "0f(%");
        assert_eq!(chat.cursor.unwrap().byte, "  alpha 中文beta (x".len());
        read(&mut chat, "y%");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("(x)")]);
        read(&mut chat, "3|");
        assert_eq!(chat.cursor.unwrap().byte, 2);
        read(&mut chat, "g_");
        assert_eq!(chat.cursor.unwrap().byte, "  alpha 中文beta (x".len());
        read(&mut chat, "j0)");
        assert_eq!(
            chat.cursor.unwrap().byte,
            "  alpha 中文beta (x)  \nSecond sentence. ".len()
        );
        read(&mut chat, "(");
        assert_eq!(chat.cursor.unwrap().byte, 0);
    }

    #[test]
    fn visual_objects_extend_and_gv_restores_and_swaps_selections() {
        let (mut chat, mut fonts) = reading("alpha beta gamma");
        read(&mut chat, "viwy");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("alpha")]);
        read(&mut chat, "gvawy");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("alpha beta")]);
        read(&mut chat, "G0wviw");
        let second = chat.picked.unwrap();
        read(&mut chat, "gv");
        let first = chat.picked.unwrap();
        assert_ne!(first.anchor, second.anchor);
        read(&mut chat, "gv");
        assert_eq!(chat.picked.unwrap().anchor, second.anchor);
        read_named(&mut chat, NamedKey::Escape);
        read(&mut chat, "gvy");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("beta")]);
    }

    #[test]
    fn word_back_ends_cross_blocks_and_empty_lines_without_splitting_unicode() {
        let (mut chat, _fonts) = reading("first word\n\n中文end");
        read(&mut chat, "G0ge");
        assert_eq!(chat.cursor.unwrap().byte, "first wor".len());
        read(&mut chat, "}0w");
        assert_eq!(chat.cursor.unwrap().byte, "中文".len());
    }

    #[test]
    fn viewport_motions_and_placement_follow_proportional_rows() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), crate::chat::fixtures::long_chat(15));
        chat.reading = true;
        chat.scroll_margin = 0;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "8G");
        paint(&mut chat, &mut fonts);
        read(&mut chat, "M");
        let middle = chat.cursor.unwrap();
        read(&mut chat, "H");
        let top = chat.cursor.unwrap();
        read(&mut chat, "L");
        assert!(top <= middle && middle <= chat.cursor.unwrap());
        read(&mut chat, "Mzz");
        let cursor = chat.cursor;
        paint(&mut chat, &mut fonts);
        let centered = chat.scroll;
        read(&mut chat, "zt");
        assert!(chat.scroll > centered);
        assert_eq!(chat.cursor, cursor);
        read(&mut chat, "zb");
        assert!(chat.scroll < centered);
        assert_eq!(chat.cursor, cursor);
    }

    #[test]
    fn wrapped_row_yanks_and_saved_selections_survive_reflow_but_not_replacement() {
        let answer = "alpha beta gamma ".repeat(60);
        let (mut chat, mut fonts) = reading(&answer);
        read(&mut chat, "j0");
        let row_start = chat.cursor.unwrap().byte;
        read(&mut chat, "ll");
        let start = chat.cursor.unwrap();
        read(&mut chat, "yg0");
        assert_eq!(
            yanked(&mut chat, &mut fonts)[0].0,
            answer[row_start..start.byte]
        );
        read(&mut chat, "g$vg0y");
        let selected = yanked(&mut chat, &mut fonts);
        assert!(!selected[0].0.is_empty());
        chat.paint(&mut fonts, [0.0; 2], [450.0, 1000.0], true);
        read(&mut chat, "gvy");
        assert_eq!(yanked(&mut chat, &mut fonts), selected);
        assert_eq!(chat.cursor.unwrap().text, start.text);
        chat.cut_from(0);
        read(&mut chat, "gv");
        assert!(chat.take_failed());
        assert!(chat.picked.is_none());
    }

    #[test]
    fn reselect_restores_a_selection_in_a_turn_evicted_from_the_layout_cache() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), crate::chat::fixtures::long_chat(40));
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        read(&mut chat, "viwy");
        let copied = yanked(&mut chat, &mut fonts);
        read(&mut chat, "G");
        paint(&mut chat, &mut fonts);
        read(&mut chat, "gv");
        assert!(chat.reading_unsettled());
        chat.settle(&mut fonts);
        assert!(!chat.reading_unsettled());
        read(&mut chat, "y");
        assert_eq!(yanked(&mut chat, &mut fonts), copied);
    }

    #[test]
    fn view_placement_waits_for_a_turn_that_has_not_been_laid_out() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), crate::chat::fixtures::long_chat(40));
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "10zt");
        paint(&mut chat, &mut fonts);
        assert_eq!(chat.cursor.unwrap().text.turn, 9);
        assert!(chat.place_after_layout.is_none());
        let top = chat.visible_rows().first().unwrap().0;
        assert_eq!(top.text.turn, 9);
    }

    #[test]
    fn code_words_use_the_daemons_language_metadata_for_motions_and_objects() {
        let (mut chat, mut fonts) = reading("```css\nmargin-top: 1px;\n```");
        let asks = chat.take_colour_asks();
        assert!(!asks.is_empty());
        chat.colours_asked(Some(42), asks[0].clone());
        chat.code_words(42, vec!["-".into()]);
        read(&mut chat, "yiw");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("margin-top")]);
        read(&mut chat, "w");
        assert_eq!(chat.cursor.unwrap().byte, "margin-top".len());
    }
}
