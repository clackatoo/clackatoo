//! Tables, laid out as a grid, each cell a block of its own, wider than
//! the column if their cells are: whoever draws one scrolls it sideways.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use parley::Layout;

use super::layout::{Brush, LINE, Laid, LaidBlock, Look, Mark, Measures, lay_out_block};
use crate::fonts::Fonts;
use crate::markdown;

/// A table, laid out: a grid of its cells, each as wide as the widest in
/// its column, the head in bold over a line across.
pub(crate) struct TableLaid {
    /// Its blocks, the cells.
    pub(crate) blocks: std::ops::Range<usize>,
    /// Where it starts, from the column's left, and its top, from the top
    /// of what it belongs to.
    pub(crate) left: f32,
    pub(crate) top: f32,
    /// How wide it is, all of it: wider than the column, it scrolls
    /// sideways.
    pub(crate) width: f32,
    pub(crate) height: f32,
    /// How far down the line under its head is, from its top.
    pub(crate) rule: f32,
    /// The marks of a side where it's cut off, scrolled: `‹` and `›`.
    pub(crate) marks: [Mark; 2],
}

/// Blocks, tables' cells among them, and marks laid out, by what they were
/// laid out from, hashed (see [`LaidBlock`]'s `key`): several of the same,
/// as a list's markers and many of its items are.
#[derive(Default)]
pub(crate) struct Cells(HashMap<u64, Vec<Layout<Brush>>>);

impl Cells {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// One laid out from what's hashed as `key`, if one's kept.
    pub(crate) fn remove(&mut self, key: u64) -> Option<Layout<Brush>> {
        let kept = self.0.get_mut(&key)?;
        let layout = kept.pop();
        if kept.is_empty() {
            self.0.remove(&key);
        }
        layout
    }

    /// Keeps `layout`, laid out from what's hashed as `key`.
    pub(crate) fn insert(&mut self, key: u64, layout: Layout<Brush>) {
        self.0.entry(key).or_default().push(layout);
    }

    /// How many are kept.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.values().map(Vec::len).sum()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<(u64, Layout<Brush>)> for Cells {
    fn from_iter<I: IntoIterator<Item = (u64, Layout<Brush>)>>(laid: I) -> Self {
        let mut cells = Self::new();
        for (key, layout) in laid {
            cells.insert(key, layout);
        }
        cells
    }
}

impl Laid {
    /// Its blocks and marks, laid out, to lay it out again with, as more
    /// of it comes.
    pub(crate) fn into_cells(self) -> Cells {
        let mut cells = Cells::new();
        for block in self.blocks {
            cells.insert(block.key, block.layout);
            if let Some((mark, _)) = block.marker {
                cells.insert(mark.key, mark.layout);
            }
        }
        for mark in self.tables.into_iter().flat_map(|table| table.marks) {
            cells.insert(mark.key, mark.layout);
        }
        cells
    }
}

/// What a table's cell, `block`, is laid out from, hashed: its text, its
/// styles, and whether it's in the head.
pub(crate) fn cell_key(block: &markdown::Block, head: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    (&block.text, &block.spans, head).hash(&mut hasher);
    hasher.finish()
}

impl Laid {
    /// The number of the table block `index` is a cell of, if it's one.
    pub(crate) fn table_of(&self, index: usize) -> Option<usize> {
        (self.tables.iter()).position(|table| table.blocks.contains(&index))
    }
}

impl TableLaid {
    /// How far it can scroll sideways, in a column `column` wide.
    pub(crate) fn max_scroll(&self, column: f32) -> f32 {
        (self.width - (column - self.left)).max(0.0)
    }
}

/// The space between a table's columns.
fn table_gap(m: Measures) -> f32 {
    m.em(1.5)
}

/// Lays out a table's `cells`, as `look` says, onto the end of `laid`:
/// each on one line, its column as wide as its widest, lined up as the
/// column says; the head in bold, with a line under it.
pub(crate) fn lay_out_table(
    (layout_cx, fonts): (&mut parley::LayoutContext<Brush>, &mut Fonts),
    m: Measures,
    (look, laid_before): (Look, &mut Cells),
    cells: Vec<markdown::Block>,
    laid: &mut Laid,
) {
    let left = cells
        .first()
        .map_or(0.0, |cell| f32::from(cell.indent) * m.em(1.5));
    let laid_out: Vec<(markdown::Cell, markdown::Block, Layout<Brush>, u64)> = (cells.into_iter())
        .filter_map(|block| {
            let cell = block.kind.cell()?;
            let key = cell_key(&block, cell.row == 0);
            let layout = laid_before.remove(key).unwrap_or_else(|| {
                laid.fresh += 1;
                lay_out_block(layout_cx, fonts, (&block, &[]), look, m.em, f32::INFINITY)
            });
            Some((cell, block, layout, key))
        })
        .collect();
    let columns = laid_out
        .iter()
        .map(|(cell, ..)| cell.column + 1)
        .max()
        .unwrap_or(0);
    let rows = laid_out
        .iter()
        .map(|(cell, ..)| cell.row + 1)
        .max()
        .unwrap_or(0);
    let mut widths = vec![0.0_f32; columns];
    let mut heights = vec![0.0_f32; rows];
    for (cell, _, layout, _) in &laid_out {
        widths[cell.column] = widths[cell.column].max(layout.width());
        heights[cell.row] = heights[cell.row].max(layout.height());
    }
    // An empty row, as one of empty cells, is a line high all the same.
    let line = (m.em * LINE).round();
    for height in &mut heights {
        if *height == 0.0 {
            *height = line;
        }
    }
    let gap = table_gap(m);
    let mut xs = Vec::with_capacity(columns);
    let mut x = 0.0;
    for width in &widths {
        xs.push(x);
        x += width + gap;
    }
    let width = (x - gap).max(0.0);
    // The head, a line under it, and the rows, a little apart.
    let (under, apart) = (m.em(0.35), m.em(0.2));
    let mut ys = Vec::with_capacity(rows);
    let mut y = 0.0;
    let mut rule = 0.0;
    for (row, height) in heights.iter().enumerate() {
        ys.push(y);
        y += height;
        if row == 0 {
            rule = (y + under).round();
            y = rule + 1.0 + under;
        } else {
            y += apart;
        }
    }
    let height = (y - if rows > 1 { apart } else { 0.0 }).max(0.0);
    let top = laid.height;
    let first = laid.blocks.len();
    for (cell, block, layout, key) in laid_out {
        let room = widths[cell.column] - layout.width();
        let within = match cell.align {
            markdown::Align::Left => 0.0,
            markdown::Align::Middle => (room / 2.0).round(),
            markdown::Align::Right => room,
        };
        laid.blocks.push(LaidBlock {
            layout,
            text: block.text,
            kind: block.kind,
            left: left + xs[cell.column] + within,
            top: top + ys[cell.row],
            marker: None,
            key,
            at: block.at,
            links: block.links,
        });
    }
    laid.tables.push(TableLaid {
        blocks: first..laid.blocks.len(),
        left,
        top,
        width,
        height,
        rule,
        marks: ["‹", "›"].map(|mark| Mark::of((layout_cx, fonts), laid_before, mark, m.em)),
    });
    laid.width = laid.width.max(left + width);
    laid.height += height;
}
