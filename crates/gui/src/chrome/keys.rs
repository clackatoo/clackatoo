//! The panel of keys raised above the bottom bar while keys wait for the
//! rest of a binding, as after Ctrl+B, as which-key.nvim shows them: the
//! keys waiting as its title, then each key that can come next, in bold
//! amber, and what it does, quietly, in up to three columns.
//!
//! It sits at the bar's right, where messages too long for the bar are
//! raised, in the terminal's font, so its columns line up by cells. It
//! takes as many columns as fit the window's width, three at most, unless
//! the rows wouldn't fit above the bar, when it takes more if they fit.
//! Rows that still don't fit end with a row saying how many more there
//! are, never left out unsaid; a description too long for a narrow window
//! is cut short.

use super::{Geometry, Painted, Rect, cells_in, cut, push_shadow};
use crate::keymap::WhichKeys;
use crate::render::{Fill, Ink, Paint, Text, TextFont, mono_width};

/// The most columns the panel takes.
const COLUMNS: usize = 3;
/// The cells between a key and what it does.
const KEY_GAP: usize = 2;
/// The cells between columns.
const COLUMN_GAP: usize = 4;

/// The panel, laid out: where it is, and where its text goes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KeysPanel {
    pub(crate) rect: Rect,
    title: (String, [f32; 2]),
    /// Each row: its keys, and what they do, and where each starts.
    rows: Vec<Row>,
}

/// A row of the panel, placed.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    keys: (String, [f32; 2]),
    does: (String, [f32; 2]),
}

/// How wide, in cells, the widest of each column's keys and descriptions
/// are, `rows` in columns of `per` from the top down.
fn widths(rows: &[(String, String)], per: usize) -> Vec<(usize, usize)> {
    rows.chunks(per.max(1))
        .map(|column| {
            column.iter().fold((0, 0), |(keys, does), row| {
                (keys.max(mono_width(&row.0)), does.max(mono_width(&row.1)))
            })
        })
        .collect()
}

/// How many cells columns of `widths` take side by side.
fn total(widths: &[(usize, usize)]) -> usize {
    let columns: usize = (widths.iter())
        .map(|(keys, does)| keys + KEY_GAP + does)
        .sum();
    columns + COLUMN_GAP * widths.len().saturating_sub(1)
}

impl KeysPanel {
    /// Lays out `which` above the bottom bar of the window `geometry`
    /// lays out, at its right.
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn new(geometry: &Geometry, which: WhichKeys) -> Self {
        let metrics = geometry.metrics;
        let padding = metrics.padding;
        let [cell_width, cell_height] = metrics.cell;
        let [width, _] = geometry.size;
        // The cells the text has inside the box, and the rows above the bar.
        let room = cells_in((width - 10.0 * padding).max(cell_width), cell_width);
        let fit = cells_in(
            geometry.bottom.y - geometry.top.height - 8.0 * padding - 2.0 * cell_height,
            cell_height,
        )
        .max(1);
        let WhichKeys {
            title, mut rows, ..
        } = which;
        // As many columns as fit, three at most: fewer rows, less in the way.
        let mut per = rows.len();
        for columns in (1..=COLUMNS).rev() {
            per = rows.len().div_ceil(columns).max(1);
            if total(&widths(&rows, per)) <= room {
                break;
            }
        }
        // Taller than fits above the bar: more columns, while they fit.
        while per > fit {
            let more = rows.len().div_ceil(per) + 1;
            let shorter = rows.len().div_ceil(more).max(1);
            if shorter >= per || total(&widths(&rows, shorter)) > room {
                break;
            }
            per = shorter;
        }
        // Still too tall: the last row that fits says how many more there
        // are.
        if per > fit {
            let columns = rows.len().div_ceil(per);
            let kept = (fit * columns).saturating_sub(1);
            let more = rows.len() - kept;
            rows.truncate(kept);
            rows.push(("…".to_owned(), format!("{more} more")));
            per = fit;
        }
        // In one column too wide for the window, what keys do is cut short.
        let mut widths = widths(&rows, per);
        if total(&widths) > room
            && let [(keys, does)] = widths.as_mut_slice()
        {
            *does = room.saturating_sub(*keys + KEY_GAP).max(1);
            for row in &mut rows {
                row.1 = cut(&row.1, *does);
            }
        }
        let inner = total(&widths).max(mono_width(&title));
        let box_width = (inner as f32 * cell_width + 6.0 * padding).round();
        let height = ((per + 1) as f32 * cell_height + cell_height / 2.0 + 3.0 * padding).round();
        let rect = Rect {
            x: (width - 2.0 * padding - box_width)
                .max(2.0 * padding)
                .round(),
            y: (geometry.bottom.y - height - 2.0 * padding).round(),
            width: box_width,
            height,
        };
        let left = rect.x + 3.0 * padding;
        let top = rect.y + 1.5 * padding;
        let first = (top + cell_height * 1.5).round();
        let mut placed = Vec::with_capacity(rows.len());
        let mut x = left;
        for (column, (keys_width, does_width)) in rows.chunks(per).zip(&widths) {
            for (index, (keys, does)) in column.iter().enumerate() {
                let y = (first + index as f32 * cell_height).round();
                placed.push(Row {
                    keys: (keys.clone(), [x.round(), y]),
                    does: (
                        does.clone(),
                        [(x + (keys_width + KEY_GAP) as f32 * cell_width).round(), y],
                    ),
                });
            }
            x += (keys_width + KEY_GAP + does_width + COLUMN_GAP) as f32 * cell_width;
        }
        Self {
            rect,
            title: (title, [left.round(), top.round()]),
            rows: placed,
        }
    }

    /// Draws the panel over the panes, as the palette is.
    pub(crate) fn paint<'a>(&'a self, geometry: &Geometry, painted: &mut Painted<'a>) {
        let rect = self.rect;
        push_shadow(
            &mut painted.over,
            rect,
            geometry.radius,
            geometry.metrics.padding,
        );
        painted.over.push(
            Fill::rounded(rect.to_array(), Paint::Palette, geometry.radius)
                .bordered(geometry.edge, Paint::PaletteEdge),
        );
        let text = |(content, origin): &'a (String, [f32; 2]), ink| Text {
            origin: *origin,
            content,
            ink,
            font: TextFont::Mono,
        };
        painted.over_texts.push(text(&self.title, Ink::Bold));
        for row in &self.rows {
            painted.over_texts.push(text(&row.keys, Ink::Key));
            painted.over_texts.push(text(&row.does, Ink::Muted));
        }
    }

    /// Each row's keys and what they do, as laid out.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> Vec<(&str, &str)> {
        (self.rows.iter())
            .map(|row| (row.keys.0.as_str(), row.does.0.as_str()))
            .collect()
    }

    /// Where each row's keys start.
    #[cfg(test)]
    pub(crate) fn key_origins(&self) -> Vec<[f32; 2]> {
        self.rows.iter().map(|row| row.keys.1).collect()
    }
}
