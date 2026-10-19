//! A chat's tables, laid out as a grid (see [`crate::markdown::tables`]),
//! scrolled sideways when wider than the column, by Shift and the wheel,
//! `zl` and `zh`, or the cursor going to a cell out of view.

use super::TableLaid;
use crate::chat::ChatView;
use crate::chat::paint::TextKey;
#[cfg(test)]
use crate::markdown::tables::{Cells, cell_key};

impl ChatView {
    /// How far table `table` of piece `piece` of turn `turn` is scrolled
    /// sideways, as far as it can be, as laid out.
    pub(super) fn table_scroll(&self, turn: usize, piece: usize, table: usize) -> f32 {
        let Some(&scroll) = self.table_scrolls.get(&(turn, piece, table)) else {
            return 0.0;
        };
        let most = (self.laid_table(turn, piece, table))
            .map_or(0.0, |laid| laid.max_scroll(self.cache.width));
        scroll.clamp(0.0, most)
    }

    /// Table `table` of piece `piece` of turn `turn`, as laid out, if it
    /// is.
    fn laid_table(&self, turn: usize, piece: usize, table: usize) -> Option<&TableLaid> {
        let laid = self.cache.turns.get(turn)?.laid.as_ref()?;
        laid.pieces.get(&piece)?.tables.get(table)
    }

    /// Scrolls the table at `at`, from the pane's top left, or with none,
    /// the one the cursor's in, `pixels` to the right, or the left if
    /// fewer than none. Returns whether it moved.
    pub(crate) fn scroll_sideways(&mut self, at: Option<[f32; 2]>, pixels: f32) -> bool {
        let found = match at {
            Some(at) => self.table_at(at),
            None => self.cursor.and_then(|cursor| {
                let TextKey { turn, piece, part } = cursor.text;
                let laid = self.cache.turns.get(turn)?.laid.as_ref()?;
                Some((turn, piece, laid.pieces.get(&piece)?.table_of(part)?))
            }),
        };
        let Some((turn, piece, table)) = found else {
            return false;
        };
        let most = (self.laid_table(turn, piece, table))
            .map_or(0.0, |laid| laid.max_scroll(self.cache.width));
        let old = self.table_scroll(turn, piece, table);
        let new = (old + pixels).clamp(0.0, most);
        if new > 0.0 {
            self.table_scrolls.insert((turn, piece, table), new);
        } else {
            self.table_scrolls.remove(&(turn, piece, table));
        }
        (new - old).abs() > f32::EPSILON
    }

    /// Scrolls the table at `at`, from the pane's top left, sideways, as
    /// the wheel turned `lines` with Shift held does: up, to the left.
    /// `None` if there's no table there to scroll, or it fits.
    pub(crate) fn wheel_sideways(&mut self, at: [f32; 2], lines: f32) -> Option<bool> {
        let (turn, piece, table) = self.table_at(at)?;
        let laid = self.laid_table(turn, piece, table)?;
        if laid.max_scroll(self.cache.width) <= 0.0 {
            return None;
        }
        Some(self.scroll_sideways(Some(at), -lines * self.line_height))
    }

    /// The table at `at`, from the pane's top left, as drawn last: its
    /// turn, piece, and number in it.
    fn table_at(&self, at: [f32; 2]) -> Option<(usize, usize, usize)> {
        let left = self.column_right - self.cache.width;
        self.shown.iter().find_map(|shown| {
            let TextKey { turn, piece, part } = shown.key;
            let laid = self.cache.turns.get(turn)?.laid.as_ref()?;
            let item = laid.pieces.get(&piece)?;
            let number = item.table_of(part)?;
            let (table, block) = (item.tables.get(number)?, item.blocks.get(part)?);
            let top = shown.at[1] - block.top + table.top;
            let rect = [
                left + table.left,
                top,
                self.cache.width - table.left,
                table.height,
            ];
            let inside = at[0] >= rect[0]
                && at[0] < rect[0] + rect[2]
                && at[1] >= rect[1]
                && at[1] < rect[1] + rect[3];
            inside.then_some((turn, piece, number))
        })
    }

    /// From text `key`, if it's a table's cell: the cell in the row above,
    /// or with `up` false, below, in the same column, or the nearest in
    /// that row if that one's empty; or if there's no such row, `Err` and
    /// the table's first cell, or its last, to go on from.
    pub(in crate::chat) fn table_step(
        &self,
        key: TextKey,
        up: bool,
    ) -> Option<Result<TextKey, TextKey>> {
        let laid = self.cache.turns.get(key.turn)?.laid.as_ref()?;
        let item = laid.pieces.get(&key.piece)?;
        let table = item.tables.get(item.table_of(key.part)?)?;
        let here = item.blocks.get(key.part)?.kind.cell()?;
        let cells = || {
            table
                .blocks
                .clone()
                .filter_map(|part| Some((part, item.blocks[part].kind.cell()?)))
        };
        let row = if up {
            cells()
                .map(|(_, cell)| cell.row)
                .filter(|&row| row < here.row)
                .max()
        } else {
            cells()
                .map(|(_, cell)| cell.row)
                .filter(|&row| row > here.row)
                .min()
        };
        let Some(row) = row else {
            let edge = if up {
                table.blocks.start
            } else {
                table.blocks.end - 1
            };
            return Some(Err(TextKey { part: edge, ..key }));
        };
        let (part, _) = (cells().filter(|(_, cell)| cell.row == row))
            .min_by_key(|(_, cell)| cell.column.abs_diff(here.column))?;
        Some(Ok(TextKey { part, ..key }))
    }

    /// Scrolls the table the cursor's in sideways, if it's in one, so the
    /// cell it's on shows.
    pub(super) fn reveal_cell(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let TextKey { turn, piece, part } = cursor.text;
        let Some(laid) = self
            .cache
            .turns
            .get(turn)
            .and_then(|slot| slot.laid.as_ref())
        else {
            return;
        };
        let Some(item) = laid.pieces.get(&piece) else {
            return;
        };
        let (Some(number), Some(block)) = (item.table_of(part), item.blocks.get(part)) else {
            return;
        };
        let Some(table) = item.tables.get(number) else {
            return;
        };
        let room = self.cache.width - table.left;
        let (start, width) = (block.left - table.left, block.layout.width());
        let scroll = self.table_scroll(turn, piece, number);
        let scroll = if start < scroll {
            start
        } else if start + width > scroll + room {
            (start + width - room).min(start)
        } else {
            return;
        };
        if scroll > 0.0 {
            self.table_scrolls.insert((turn, piece, number), scroll);
        } else {
            self.table_scrolls.remove(&(turn, piece, number));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::laid_and_drawn;
    use super::super::{ITEMS, LaidBlock, Measures};
    use super::{Cells, cell_key};
    use crate::chat::ChatView;
    use crate::fonts::{FontOptions, Fonts};
    use crate::render::Paint;

    #[test]
    fn a_table_is_a_grid_with_its_head_in_bold_over_a_line() {
        let markdown = "| name | kind | size |\n|---|:-:|--:|\n| parser.rs | source | 12 KiB |\n| a much longer name | x | 1 |";
        let (laid, paint) = laid_and_drawn(markdown);
        assert_eq!(laid.tables.len(), 1);
        let table = &laid.tables[0];
        assert_eq!(table.blocks, 0..9);
        let cell = |row: usize, column: usize| {
            (laid.blocks.iter())
                .find(|block| {
                    block
                        .kind
                        .cell()
                        .is_some_and(|cell| (cell.row, cell.column) == (row, column))
                })
                .unwrap()
        };
        let right = |block: &LaidBlock| block.left + block.layout.width();
        let middle = |block: &LaidBlock| block.left + block.layout.width() / 2.0;
        // Lined up as each column says: from its left, its middle, its
        // right, at the left of the text around it.
        for row in 0..3 {
            assert!(cell(row, 0).left.abs() < f32::EPSILON);
            assert!((middle(cell(row, 1)) - middle(cell(0, 1))).abs() <= 1.0);
            assert!((right(cell(row, 2)) - right(cell(0, 2))).abs() < 0.01);
        }
        // Each column as wide as its widest cell, and a gap.
        let m = Measures::of(16.0);
        assert!(cell(0, 1).left > right(cell(2, 0)) + m.em(1.0));
        assert!((table.width - right(cell(0, 2))).abs() < 0.01);
        // The head over a line, then the rows: one a line.
        assert!(cell(0, 0).top < table.rule && table.rule < cell(1, 0).top);
        assert!(cell(1, 0).top < cell(2, 0).top);
        let rule = (paint.fills.iter())
            .find(|fill| fill.paint == Paint::Divider)
            .expect("a line under the head");
        assert!((rule.rect[0] - 100.0).abs() < f32::EPSILON);
        assert!((rule.rect[2] - table.width).abs() < 0.01);
        assert!(rule.rect[3] <= 1.0);
        // The head in bold: wider than the same word in a row.
        let (word, _) = laid_and_drawn("| word |\n|---|\n| word |");
        assert!(word.blocks[0].layout.width() > word.blocks[1].layout.width());
    }

    #[test]
    fn a_table_too_wide_scrolls_sideways_by_the_wheel_and_the_keys() {
        use crate::chat::fixtures::{paint as draw, read, talk};
        let wide = (0..12)
            .map(|n| format!("column number {n}"))
            .collect::<Vec<_>>();
        let markdown = format!(
            "| {} |\n|{}\n| {} |",
            wide.join(" | "),
            "---|".repeat(12),
            wide.join(" | ")
        );
        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let mut chat = ChatView::new(String::new(), talk(&[("show me", &markdown)]));
        draw(&mut chat, &mut fonts);
        let table = |chat: &ChatView| {
            let laid = chat.cache.turns[0].laid.as_ref().unwrap();
            let table = &laid.pieces[&ITEMS].tables[0];
            (
                table.max_scroll(chat.cache.width),
                chat.table_scroll(0, ITEMS, 0),
            )
        };
        let (most, scroll) = table(&chat);
        assert!(most > 0.0 && scroll == 0.0);
        // Shift and the wheel, over it: down goes right; not past its end.
        let cell = (chat.shown.iter())
            .find(|shown| shown.key.piece == ITEMS)
            .copied()
            .unwrap();
        let at = [cell.at[0] + 4.0, cell.at[1] + 4.0];
        assert_eq!(chat.wheel_sideways(at, -1.0), Some(true));
        assert!(table(&chat).1 > 0.0);
        assert_eq!(chat.wheel_sideways(at, -1000.0), Some(true));
        assert!((table(&chat).1 - most).abs() < 0.01);
        assert_eq!(chat.wheel_sideways(at, -1.0), Some(false));
        // What's drawn, and where the mouse finds it, moved with it.
        draw(&mut chat, &mut fonts);
        let moved = (chat.shown.iter())
            .find(|shown| shown.key == cell.key)
            .unwrap();
        assert!((cell.at[0] - moved.at[0] - most).abs() < 0.01);
        // Not over a table: the wheel scrolls as ever.
        assert_eq!(chat.wheel_sideways([1.0, 1.0], -1.0), None);
        // In normal mode, `zH` goes back, and moving the cursor to a cell
        // out of view brings it into view.
        chat.reading = true;
        draw(&mut chat, &mut fonts);
        read(&mut chat, "zH");
        assert!(table(&chat).1 < most);
        read(&mut chat, "zH");
        read(&mut chat, "zH");
        read(&mut chat, "zH");
        assert!(table(&chat).1 < 0.01);
        // With the cursor on the first cell, scrolled out of the column,
        // it isn't drawn over the margin.
        read(&mut chat, "gg}");
        draw(&mut chat, &mut fonts);
        let first = chat.cursor.unwrap().text;
        assert_eq!(chat.table_step(first, true), Some(Err(first)));
        read(&mut chat, "zl");
        let drawn = draw(&mut chat, &mut fonts);
        let left = chat.column_right - chat.cache.width;
        assert!(table(&chat).1 > 0.0);
        let shown_from = |fill: &crate::render::Fill| {
            let [x, _, w, _] = fill.rect;
            let [cx, _, cw, _] = fill.clip.unwrap_or([f32::MIN, 0.0, f32::MAX, 0.0]);
            let (from, to) = (x.max(cx), (x + w).min(cx + cw));
            (to > from).then_some(from)
        };
        assert!(
            (drawn.over.iter())
                .filter(|fill| fill.paint == Paint::Accent)
                .filter_map(shown_from)
                .all(|from| from >= left),
            "{:?}",
            drawn.over
        );
        read(&mut chat, "G");
        draw(&mut chat, &mut fonts);
        let cursor = chat.cursor.unwrap().text;
        let last = (chat.shown.iter())
            .find(|shown| shown.key == cursor)
            .unwrap();
        assert!(table(&chat).1 > most / 2.0);
        assert!(
            last.at[0] + last.size[0] <= chat.column_right + 0.01,
            "the last cell shows"
        );
        // Scrolled back to its start, nothing is kept of it.
        chat.scroll_sideways(None, -f32::MAX);
        assert!(chat.table_scrolls.is_empty());
        // Scrolled to its end, a narrower column keeps it where it was,
        // and a wider one, back to as far as it can go then.
        chat.scroll_sideways(None, f32::MAX);
        let (wide, _) = table(&chat);
        chat.paint(&mut fonts, [0.0; 2], [600.0, 1000.0], true);
        let (narrow, scroll) = table(&chat);
        assert!(narrow > wide && (scroll - wide).abs() < 0.01);
        chat.scroll_sideways(None, f32::MAX);
        draw(&mut chat, &mut fonts);
        assert!((table(&chat).1 - wide).abs() < 0.01);
    }

    #[test]
    fn a_cut_off_table_is_marked_where_its_cut_off_and_its_cells_kept_as_it_grows() {
        use crate::chat::fixtures::{paint as draw, talk};

        let wide = (0..12).map(|n| format!("column {n}")).collect::<Vec<_>>();
        let head = format!("| {} |\n|{}", wide.join(" | "), "---|".repeat(12));
        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let mut chat = ChatView::new(String::new(), talk(&[("show me", &head)]));
        let strip = Measures::of(super::super::text_size(&fonts)).em(1.2);
        let strips = |paint: &crate::chat::TextPaint| {
            (paint.fills.iter())
                .filter(|fill| fill.paint == Paint::Tile && (fill.rect[2] - strip).abs() < 0.01)
                .map(|fill| fill.rect[0])
                .collect::<Vec<_>>()
        };
        // At its start, cut off on the right; part way, on both sides; at
        // its end, on the left.
        let at_start = strips(&draw(&mut chat, &mut fonts));
        assert_eq!(at_start.len(), 1);
        let at = table_at_its_cell(&chat);
        chat.scroll_sideways(at, 40.0);
        assert_eq!(strips(&draw(&mut chat, &mut fonts)).len(), 2);
        chat.scroll_sideways(at, f32::MAX);
        let at_end = strips(&draw(&mut chat, &mut fonts));
        assert_eq!(at_end.len(), 1);
        assert!(at_end[0] < at_start[0]);
        // As a row comes, the head's cells are kept, not laid out again.
        let laid = &chat.cache.turns[0].laid.as_ref().unwrap().pieces[&ITEMS];
        let mut cells: Cells = (laid.blocks.iter())
            .map(|block| (block.key, block.layout.clone()))
            .collect();
        let grown = format!("{head}\n| {} |", wide.join(" | "));
        let mut layout_cx = parley::LayoutContext::new();
        let m = Measures::of(super::super::text_size(&fonts));
        let words = (grown.as_str(), super::super::Look::Words);
        let width = (chat.cache.width, &mut cells);
        let again =
            super::super::lay_out_coloured(&mut layout_cx, &mut fonts, m, words, width, None);
        assert!(cells.is_empty(), "every cell of the head was kept");
        assert_eq!(again.blocks.len(), 24);
        // A cell in bold, or in the head, is another cell to keep.
        let block = &again.blocks[0];
        let markdown = crate::chat::markdown::blocks("| **column 0** |\n|---|");
        assert_ne!(cell_key(&markdown[0], true), block.key);
        assert_ne!(cell_key(&markdown[0], false), cell_key(&markdown[0], true));
    }

    /// Where the first table `chat` drew is, from the pane's top left.
    fn table_at_its_cell(chat: &ChatView) -> Option<[f32; 2]> {
        let cell = (chat.shown.iter()).find(|shown| shown.key.piece == ITEMS)?;
        Some([cell.at[0] + 2.0, cell.at[1] + 2.0])
    }

    #[test]
    fn up_and_down_in_a_table_go_to_the_cell_above_and_below() {
        use crate::chat::fixtures::{paint as draw, read, talk};

        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let answer = "Before.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n|  | 4 |\n\nAfter.";
        let mut chat = ChatView::new(String::new(), talk(&[("show me", answer)]));
        chat.reading = true;
        draw(&mut chat, &mut fonts);
        let text = |chat: &ChatView| {
            let cursor = chat.cursor.unwrap();
            chat.text(cursor.text).unwrap().text.to_owned()
        };
        // From the end, up the second column, then off the table's top.
        read(&mut chat, "G");
        assert_eq!(text(&chat), "After.");
        read(&mut chat, "k");
        assert_eq!(text(&chat), "4");
        read(&mut chat, "k");
        assert_eq!(text(&chat), "2");
        read(&mut chat, "k");
        assert_eq!(text(&chat), "b");
        read(&mut chat, "k");
        assert_eq!(text(&chat), "Before.");
        // Down the first, whose last cell is empty: the nearest in its row.
        read(&mut chat, "j");
        assert_eq!(text(&chat), "a");
        read(&mut chat, "jj");
        assert_eq!(text(&chat), "4");
        read(&mut chat, "j");
        assert_eq!(text(&chat), "After.");
    }
}
