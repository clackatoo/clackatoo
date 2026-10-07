//! Where a tab's panes go in the window, in cells, and which pane is next
//! to which.
//!
//! A split shares its space equally between its parts, with a column or
//! row of cells between each two for the divider. Space that doesn't
//! divide equally goes to the first parts.

use clackatoo_protocol::{Axis, Layout, PaneId};

/// A rectangle of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cells {
    pub(crate) col: u16,
    pub(crate) row: u16,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl Cells {
    fn end_col(self) -> u16 {
        self.col + self.cols
    }

    fn end_row(self) -> u16 {
        self.row + self.rows
    }
}

/// A divider between two parts of a split: a column of cells, for parts
/// side by side, or a row, for parts stacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Divider {
    /// Which way the parts it divides are side by side.
    pub(crate) axis: Axis,
    pub(crate) cells: Cells,
}

/// Where everything in a tab goes.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Placement {
    pub(crate) panes: Vec<(PaneId, Cells)>,
    pub(crate) dividers: Vec<Divider>,
}

impl Placement {
    /// Lays out `layout` in `area`.
    pub(crate) fn new(layout: &Layout, area: Cells) -> Self {
        let mut placement = Self::default();
        placement.place(layout, area);
        placement
    }

    fn place(&mut self, layout: &Layout, area: Cells) {
        let (axis, parts) = match layout {
            Layout::Pane(pane) => {
                self.panes.push((pane.id, area));
                return;
            }
            Layout::Split { axis, parts } => (*axis, parts),
        };
        let length = match axis {
            Axis::LeftToRight => area.cols,
            Axis::TopToBottom => area.rows,
        };
        let count = u16::try_from(parts.len()).unwrap_or(u16::MAX);
        let space = length.saturating_sub(count - 1);
        let (each, extra) = (space / count, space % count);
        let mut start = 0;
        for (index, part) in (0..count).zip(parts) {
            // Every part gets a cell, even when they don't fit.
            let size = (each + u16::from(index < extra)).max(1);
            let cells = match axis {
                Axis::LeftToRight => Cells {
                    col: area.col + start,
                    cols: size,
                    ..area
                },
                Axis::TopToBottom => Cells {
                    row: area.row + start,
                    rows: size,
                    ..area
                },
            };
            self.place(part, cells);
            start += size;
            if index + 1 < count {
                let cells = match axis {
                    Axis::LeftToRight => Cells {
                        col: area.col + start,
                        cols: 1,
                        ..area
                    },
                    Axis::TopToBottom => Cells {
                        row: area.row + start,
                        rows: 1,
                        ..area
                    },
                };
                self.dividers.push(Divider { axis, cells });
                start += 1;
            }
        }
    }

    pub(crate) fn cells(&self, pane: PaneId) -> Option<Cells> {
        self.panes
            .iter()
            .find(|(id, _)| *id == pane)
            .map(|&(_, cells)| cells)
    }

    /// The pane next to `from` towards `direction`: of those touching its
    /// side, the one level with its top or left, or failing that the
    /// nearest.
    pub(crate) fn neighbour(&self, from: PaneId, direction: Direction) -> Option<PaneId> {
        let from_cells = self.cells(from)?;
        self.panes
            .iter()
            .filter(|&&(id, _)| id != from)
            .filter_map(|&(id, cells)| {
                // How far it is, and how far its side is from being level
                // with ours, along the side they share.
                let (gap, offset) = match direction {
                    Direction::Left if cells.end_col() <= from_cells.col => (
                        from_cells.col - cells.end_col(),
                        overlap(
                            cells.row,
                            cells.end_row(),
                            from_cells.row,
                            from_cells.end_row(),
                        )?,
                    ),
                    Direction::Right if cells.col >= from_cells.end_col() => (
                        cells.col - from_cells.end_col(),
                        overlap(
                            cells.row,
                            cells.end_row(),
                            from_cells.row,
                            from_cells.end_row(),
                        )?,
                    ),
                    Direction::Up if cells.end_row() <= from_cells.row => (
                        from_cells.row - cells.end_row(),
                        overlap(
                            cells.col,
                            cells.end_col(),
                            from_cells.col,
                            from_cells.end_col(),
                        )?,
                    ),
                    Direction::Down if cells.row >= from_cells.end_row() => (
                        cells.row - from_cells.end_row(),
                        overlap(
                            cells.col,
                            cells.end_col(),
                            from_cells.col,
                            from_cells.end_col(),
                        )?,
                    ),
                    _ => return None,
                };
                Some(((gap, offset), id))
            })
            .min()
            .map(|(_, id)| id)
    }
}

/// Which way to move between panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// If `start..end` overlaps `from_start..from_end`, how far past
/// `from_start` it starts: 0 if it starts level or before.
fn overlap(start: u16, end: u16, from_start: u16, from_end: u16) -> Option<u16> {
    (start < from_end && end > from_start).then(|| start.saturating_sub(from_start))
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Content, Pane, TerminalId};

    use super::*;

    fn pane(id: u32) -> Layout {
        Layout::Pane(Pane {
            id: PaneId(id),
            content: Content::Terminal(TerminalId(id)),
        })
    }

    fn split(axis: Axis, parts: Vec<Layout>) -> Layout {
        Layout::Split { axis, parts }
    }

    fn area(cols: u16, rows: u16) -> Cells {
        Cells {
            col: 0,
            row: 0,
            cols,
            rows,
        }
    }

    fn cells(col: u16, row: u16, cols: u16, rows: u16) -> Cells {
        Cells {
            col,
            row,
            cols,
            rows,
        }
    }

    #[test]
    fn splits_share_space_equally_around_their_dividers() {
        // (1 | (2 / 3)) in 21 x 9: 10 columns, a divider, 10 columns; 4
        // rows, a divider, 4 rows.
        let layout = split(
            Axis::LeftToRight,
            vec![pane(1), split(Axis::TopToBottom, vec![pane(2), pane(3)])],
        );
        let placement = Placement::new(&layout, area(21, 9));
        assert_eq!(
            placement.panes,
            [
                (PaneId(1), cells(0, 0, 10, 9)),
                (PaneId(2), cells(11, 0, 10, 4)),
                (PaneId(3), cells(11, 5, 10, 4)),
            ]
        );
        assert_eq!(
            placement.dividers,
            [
                Divider {
                    axis: Axis::LeftToRight,
                    cells: cells(10, 0, 1, 9)
                },
                Divider {
                    axis: Axis::TopToBottom,
                    cells: cells(11, 4, 10, 1)
                },
            ]
        );
    }

    #[test]
    fn space_left_over_goes_to_the_first_parts() {
        let layout = split(Axis::LeftToRight, vec![pane(1), pane(2), pane(3)]);
        let placement = Placement::new(&layout, area(12, 1));
        let widths: Vec<u16> = placement.panes.iter().map(|(_, c)| c.cols).collect();
        assert_eq!(widths, [4, 3, 3]);
    }

    #[test]
    fn neighbours_are_found_by_direction() {
        // 1 | 2
        //   | -
        //   | 3
        let layout = split(
            Axis::LeftToRight,
            vec![pane(1), split(Axis::TopToBottom, vec![pane(2), pane(3)])],
        );
        let placement = Placement::new(&layout, area(21, 9));
        let next = |from, direction| placement.neighbour(PaneId(from), direction);
        assert_eq!(
            next(1, Direction::Right),
            Some(PaneId(2)),
            "level with its top"
        );
        assert_eq!(next(3, Direction::Left), Some(PaneId(1)));
        assert_eq!(next(2, Direction::Down), Some(PaneId(3)));
        assert_eq!(next(3, Direction::Up), Some(PaneId(2)));
        assert_eq!(next(1, Direction::Left), None);
        assert_eq!(next(2, Direction::Up), None);
    }
}
