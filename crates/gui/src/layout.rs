//! Where a tab's panes go in the window, and which pane is next to which.
//!
//! Each pane is a tile: a rectangle beside the others, a line apart,
//! holding its terminal's cells a little in from its edges. A split shares
//! its space between its parts by their shares, in whole pixels, with any
//! pixels left over going to the first parts. The lines between a split's
//! parts are its dividers, which dragging moves.

use clackatoo_protocol::{Axis, Layout, PaneId};

/// A rectangle in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl Rect {
    pub(crate) fn contains(self, [x, y]: [f32; 2]) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    pub(crate) fn to_array(self) -> [f32; 4] {
        [self.x, self.y, self.width, self.height]
    }

    fn right(self) -> f32 {
        self.x + self.width
    }

    fn bottom(self) -> f32 {
        self.y + self.height
    }
}

/// The space a tab's tiles share, and how they're spaced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Area {
    pub(crate) rect: Rect,
    /// A cell's width and height.
    pub(crate) cell: [f32; 2],
    /// The space between tiles.
    pub(crate) gap: f32,
    /// How far in from a tile's edges its cells start.
    pub(crate) inset: f32,
}

/// One pane's tile, and the cells it holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tile {
    pub(crate) rect: Rect,
    /// Where its cells start.
    pub(crate) origin: [f32; 2],
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl Tile {
    /// The pixels its cells cover.
    pub(crate) fn cells_size(&self, cell: [f32; 2]) -> [f32; 2] {
        [
            f32::from(self.cols) * cell[0],
            f32::from(self.rows) * cell[1],
        ]
    }
}

/// How far either side of a divider's gap still grabs it, in pixels.
const GRAB: f32 = 3.0;

/// The gap between two parts of a split, which dragging moves.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Divider {
    /// Where the split is in the tab's layout: see [`Layout::at`].
    pub(crate) split: Vec<u16>,
    /// The divider follows this part of the split.
    pub(crate) after: usize,
    pub(crate) axis: Axis,
    /// The gap, and a little either side, where it can be grabbed.
    pub(crate) rect: Rect,
    /// Where each part of the split starts, and its length, along the axis.
    pub(crate) parts: Vec<(f32, f32)>,
}

impl Divider {
    /// The shares that put this divider at `position` along its axis, in
    /// whole cells from where the part before it starts, leaving both parts
    /// beside it at least a cell inside their insets.
    pub(crate) fn shares_at(&self, position: f32, area: &Area) -> Vec<u32> {
        let cell = match self.axis {
            Axis::LeftToRight => area.cell[0],
            Axis::TopToBottom => area.cell[1],
        };
        let (start, before) = self.parts[self.after];
        let after = self.parts[self.after + 1].1;
        let both = before + after;
        let least = cell + 2.0 * area.inset;
        let mut lengths: Vec<f32> = self.parts.iter().map(|&(_, length)| length).collect();
        if both >= 2.0 * least {
            // In whole cells, so the shares change only as cells do.
            let wanted = position - area.gap / 2.0 - start - 2.0 * area.inset;
            let snapped = (wanted / cell).round() * cell + 2.0 * area.inset;
            let length = snapped.clamp(least, both - least);
            lengths[self.after] = length;
            lengths[self.after + 1] = both - length;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        lengths
            .iter()
            .map(|length| ((length * 16.0).round() as u32).max(1))
            .collect()
    }

    /// Where the divider is now, along its axis: the middle of its gap.
    pub(crate) fn position(&self, area: &Area) -> f32 {
        let (start, length) = self.parts[self.after];
        start + length + area.gap / 2.0
    }
}

/// Where everything in a tab goes.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Placement {
    pub(crate) panes: Vec<(PaneId, Tile)>,
    pub(crate) dividers: Vec<Divider>,
}

impl Placement {
    /// Lays out `layout` in `area`.
    pub(crate) fn new(layout: &Layout, area: &Area) -> Self {
        let mut placement = Self::default();
        placement.place(layout, area.rect, area, &mut Vec::new());
        placement
    }

    fn place(&mut self, layout: &Layout, rect: Rect, area: &Area, path: &mut Vec<u16>) {
        let (axis, parts, shares) = match layout {
            Layout::Pane(pane) => {
                self.panes.push((pane.id, tile(rect, area)));
                return;
            }
            Layout::Split {
                axis,
                parts,
                shares,
            } => (*axis, parts, shares),
        };
        let (origin, length) = match axis {
            Axis::LeftToRight => (rect.x, rect.width),
            Axis::TopToBottom => (rect.y, rect.height),
        };
        let lengths = share_out(length, area.gap, shares);
        let mut extents = Vec::with_capacity(parts.len());
        let mut start = origin;
        for (index, (part, &size)) in parts.iter().zip(&lengths).enumerate() {
            let part_rect = match axis {
                Axis::LeftToRight => Rect {
                    x: start,
                    width: size,
                    ..rect
                },
                Axis::TopToBottom => Rect {
                    y: start,
                    height: size,
                    ..rect
                },
            };
            path.push(u16::try_from(index).unwrap_or(u16::MAX));
            self.place(part, part_rect, area, path);
            path.pop();
            extents.push((start, size));
            start += size + area.gap;
        }
        for after in 0..parts.len() - 1 {
            let (start, size) = extents[after];
            let gap_start = start + size - GRAB;
            let gap_length = area.gap + 2.0 * GRAB;
            let gap = match axis {
                Axis::LeftToRight => Rect {
                    x: gap_start,
                    width: gap_length,
                    ..rect
                },
                Axis::TopToBottom => Rect {
                    y: gap_start,
                    height: gap_length,
                    ..rect
                },
            };
            self.dividers.push(Divider {
                split: path.clone(),
                after,
                axis,
                rect: gap,
                parts: extents.clone(),
            });
        }
    }

    /// The divider at `point`, if any: the innermost, where they cross.
    pub(crate) fn divider_at(&self, point: [f32; 2]) -> Option<&Divider> {
        self.dividers
            .iter()
            .filter(|divider| divider.rect.contains(point))
            .max_by_key(|divider| divider.split.len())
    }

    pub(crate) fn tile(&self, pane: PaneId) -> Option<Tile> {
        self.panes
            .iter()
            .find(|(id, _)| *id == pane)
            .map(|&(_, tile)| tile)
    }

    /// The pane whose tile is at `point`, if any.
    pub(crate) fn pane_at(&self, point: [f32; 2]) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|(_, tile)| tile.rect.contains(point))
            .map(|&(id, _)| id)
    }

    /// The pane next to `from` towards `direction`: of those beside its
    /// side, the one level with its top or left, or failing that the
    /// nearest.
    pub(crate) fn neighbour(&self, from: PaneId, direction: Direction) -> Option<PaneId> {
        let from_rect = self.tile(from)?.rect;
        let mut best: Option<((f32, f32), PaneId)> = None;
        for &(id, tile) in &self.panes {
            if id == from {
                continue;
            }
            let rect = tile.rect;
            // How far it is, and how far its side is from being level
            // with ours, along the side they share.
            let found = match direction {
                Direction::Left if rect.right() <= from_rect.x => Some((
                    from_rect.x - rect.right(),
                    overlap(rect.y, rect.bottom(), from_rect.y, from_rect.bottom()),
                )),
                Direction::Right if rect.x >= from_rect.right() => Some((
                    rect.x - from_rect.right(),
                    overlap(rect.y, rect.bottom(), from_rect.y, from_rect.bottom()),
                )),
                Direction::Up if rect.bottom() <= from_rect.y => Some((
                    from_rect.y - rect.bottom(),
                    overlap(rect.x, rect.right(), from_rect.x, from_rect.right()),
                )),
                Direction::Down if rect.y >= from_rect.bottom() => Some((
                    rect.y - from_rect.bottom(),
                    overlap(rect.x, rect.right(), from_rect.x, from_rect.right()),
                )),
                _ => None,
            };
            let Some((gap, Some(offset))) = found else {
                continue;
            };
            if best.is_none_or(|(key, _)| (gap, offset) < key) {
                best = Some(((gap, offset), id));
            }
        }
        best.map(|(_, id)| id)
    }
}

/// `length` less the gaps between the parts, shared out by `shares` in
/// whole pixels, with pixels left over going to the first parts.
fn share_out(length: f32, gap: f32, shares: &[u32]) -> Vec<f32> {
    #[allow(clippy::cast_precision_loss)]
    let count = shares.len() as f32;
    let space = (length - gap * (count - 1.0)).max(0.0);
    let total: f64 = shares.iter().map(|&share| f64::from(share)).sum();
    #[allow(clippy::cast_possible_truncation)]
    let mut lengths: Vec<f32> = shares
        .iter()
        .map(|&share| (f64::from(space) * f64::from(share) / total.max(1.0)).floor() as f32)
        .collect();
    let mut extra = space - lengths.iter().sum::<f32>();
    for length in &mut lengths {
        if extra < 1.0 {
            break;
        }
        *length += 1.0;
        extra -= 1.0;
    }
    lengths
}

/// A tile filling `rect`, with as many whole cells as fit inside its inset.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn tile(rect: Rect, area: &Area) -> Tile {
    let cells = |pixels: f32, cell: f32| {
        u16::try_from(((pixels - 2.0 * area.inset) / cell).floor().max(1.0) as u32)
            .unwrap_or(u16::MAX)
    };
    Tile {
        rect,
        origin: [rect.x + area.inset, rect.y + area.inset],
        cols: cells(rect.width, area.cell[0]),
        rows: cells(rect.height, area.cell[1]),
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
fn overlap(start: f32, end: f32, from_start: f32, from_end: f32) -> Option<f32> {
    (start < from_end && end > from_start).then(|| (start - from_start).max(0.0))
}

#[cfg(test)]
// Every position here is a whole number of pixels, so exact.
#[allow(clippy::float_cmp)]
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
        let shares = vec![1; parts.len()];
        Layout::Split {
            axis,
            parts,
            shares,
        }
    }

    /// 410 × 210 pixels, with cells 10 × 20, a gap of 10 and an inset of 5.
    fn area() -> Area {
        Area {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 410.0,
                height: 210.0,
            },
            cell: [10.0, 20.0],
            gap: 10.0,
            inset: 5.0,
        }
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn splits_share_space_equally_around_their_gaps() {
        // (1 | (2 / 3)): 200 pixels, a gap, 200; then 100, a gap, 100.
        let layout = split(
            Axis::LeftToRight,
            vec![pane(1), split(Axis::TopToBottom, vec![pane(2), pane(3)])],
        );
        let placement = Placement::new(&layout, &area());
        let rects: Vec<Rect> = placement.panes.iter().map(|(_, tile)| tile.rect).collect();
        assert_eq!(
            rects,
            [
                rect(0.0, 0.0, 200.0, 210.0),
                rect(210.0, 0.0, 200.0, 100.0),
                rect(210.0, 110.0, 200.0, 100.0),
            ]
        );
        // Inside a 200 × 100 tile's inset of 5: 19 columns and 4 rows.
        let tile = placement.tile(PaneId(2)).unwrap();
        assert_eq!((tile.cols, tile.rows, tile.origin), (19, 4, [215.0, 5.0]));
    }

    #[test]
    fn pixels_left_over_go_to_the_first_parts() {
        let layout = split(Axis::LeftToRight, vec![pane(1), pane(2), pane(3)]);
        let widths = |area: &Area| -> Vec<f32> {
            Placement::new(&layout, area)
                .panes
                .iter()
                .map(|(_, tile)| tile.rect.width)
                .collect()
        };
        // 410 less two gaps is 390: 130 each.
        assert_eq!(widths(&area()), [130.0, 130.0, 130.0]);
        let mut wider = area();
        wider.rect.width = 412.0;
        assert_eq!(widths(&wider), [131.0, 131.0, 130.0]);
    }

    #[test]
    fn neighbours_are_found_by_direction_and_tiles_by_point() {
        // 1 | 2
        //   | -
        //   | 3
        let layout = split(
            Axis::LeftToRight,
            vec![pane(1), split(Axis::TopToBottom, vec![pane(2), pane(3)])],
        );
        let placement = Placement::new(&layout, &area());
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
        assert_eq!(placement.pane_at([300.0, 150.0]), Some(PaneId(3)));
        assert_eq!(placement.pane_at([205.0, 50.0]), None, "in the gap");
    }

    #[test]
    fn shares_size_the_parts() {
        let layout = Layout::Split {
            axis: Axis::LeftToRight,
            parts: vec![pane(1), pane(2)],
            shares: vec![3, 1],
        };
        let widths: Vec<f32> = Placement::new(&layout, &area())
            .panes
            .iter()
            .map(|(_, tile)| tile.rect.width)
            .collect();
        // 400 pixels besides the gap: 300 and 100.
        assert_eq!(widths, [300.0, 100.0]);
    }

    #[test]
    fn dividers_are_found_in_their_gaps_and_move_by_whole_cells() {
        // (1 | (2 / 3)): the upright divider at 200 to 210, and the flat
        // one in the right part, at 100 to 110.
        let layout = split(
            Axis::LeftToRight,
            vec![pane(1), split(Axis::TopToBottom, vec![pane(2), pane(3)])],
        );
        let area = area();
        let placement = Placement::new(&layout, &area);
        let upright = placement.divider_at([199.0, 50.0]).unwrap();
        assert_eq!((upright.split.as_slice(), upright.after), (&[][..], 0));
        assert_eq!(upright.position(&area), 205.0);
        let flat = placement.divider_at([300.0, 105.0]).unwrap();
        assert_eq!(
            (flat.split.as_slice(), flat.axis),
            (&[1][..], Axis::TopToBottom)
        );
        assert!(placement.divider_at([100.0, 100.0]).is_none());

        // Dragged to 103, the left part would be 98 pixels: 8.8 columns
        // inside its inset, so 9, which is 100 pixels with it. The right
        // gets the rest.
        assert_eq!(upright.shares_at(103.0, &area), [100 * 16, 300 * 16]);
        // Never smaller than a cell inside its inset.
        assert_eq!(upright.shares_at(-50.0, &area), [20 * 16, 380 * 16]);
        assert_eq!(upright.shares_at(900.0, &area), [380 * 16, 20 * 16]);
    }
}
