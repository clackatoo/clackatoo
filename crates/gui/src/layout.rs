//! Where a tab's panes go in the window, and which pane is next to which.
//!
//! Each pane is a card: a rectangle with a gap around it, holding its
//! terminal's cells a little in from its edges. A split shares its space
//! equally between its parts, in whole pixels, with any pixels left over
//! going to the first parts.

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

/// The space a tab's cards share, and how they're spaced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Area {
    pub(crate) rect: Rect,
    /// A cell's width and height.
    pub(crate) cell: [f32; 2],
    /// The space between cards.
    pub(crate) gap: f32,
    /// How far in from a card's edges its cells start.
    pub(crate) inset: f32,
}

/// One pane's card, and the cells it holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Card {
    pub(crate) rect: Rect,
    /// Where its cells start.
    pub(crate) origin: [f32; 2],
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl Card {
    /// The pixels its cells cover.
    pub(crate) fn cells_size(&self, cell: [f32; 2]) -> [f32; 2] {
        [
            f32::from(self.cols) * cell[0],
            f32::from(self.rows) * cell[1],
        ]
    }
}

/// Where everything in a tab goes.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Placement {
    pub(crate) panes: Vec<(PaneId, Card)>,
}

impl Placement {
    /// Lays out `layout` in `area`.
    pub(crate) fn new(layout: &Layout, area: &Area) -> Self {
        let mut placement = Self::default();
        placement.place(layout, area.rect, area);
        placement
    }

    fn place(&mut self, layout: &Layout, rect: Rect, area: &Area) {
        let (axis, parts) = match layout {
            Layout::Pane(pane) => {
                self.panes.push((pane.id, card(rect, area)));
                return;
            }
            Layout::Split { axis, parts } => (*axis, parts),
        };
        let length = match axis {
            Axis::LeftToRight => rect.width,
            Axis::TopToBottom => rect.height,
        };
        #[allow(clippy::cast_precision_loss)]
        let count = parts.len() as f32;
        let space = (length - area.gap * (count - 1.0)).max(0.0);
        let each = (space / count).floor();
        let mut extra = space - each * count;
        let mut start = 0.0;
        for part in parts {
            let size = each + if extra >= 1.0 { 1.0 } else { 0.0 };
            extra -= 1.0;
            let part_rect = match axis {
                Axis::LeftToRight => Rect {
                    x: rect.x + start,
                    width: size,
                    ..rect
                },
                Axis::TopToBottom => Rect {
                    y: rect.y + start,
                    height: size,
                    ..rect
                },
            };
            self.place(part, part_rect, area);
            start += size + area.gap;
        }
    }

    pub(crate) fn card(&self, pane: PaneId) -> Option<Card> {
        self.panes
            .iter()
            .find(|(id, _)| *id == pane)
            .map(|&(_, card)| card)
    }

    /// The pane whose card is at `point`, if any.
    pub(crate) fn pane_at(&self, point: [f32; 2]) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|(_, card)| card.rect.contains(point))
            .map(|&(id, _)| id)
    }

    /// The pane next to `from` towards `direction`: of those beside its
    /// side, the one level with its top or left, or failing that the
    /// nearest.
    pub(crate) fn neighbour(&self, from: PaneId, direction: Direction) -> Option<PaneId> {
        let from_rect = self.card(from)?.rect;
        let mut best: Option<((f32, f32), PaneId)> = None;
        for &(id, card) in &self.panes {
            if id == from {
                continue;
            }
            let rect = card.rect;
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

/// A card filling `rect`, with as many whole cells as fit inside its inset.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn card(rect: Rect, area: &Area) -> Card {
    let cells = |pixels: f32, cell: f32| {
        u16::try_from(((pixels - 2.0 * area.inset) / cell).floor().max(1.0) as u32)
            .unwrap_or(u16::MAX)
    };
    Card {
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
        Layout::Split { axis, parts }
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
        let rects: Vec<Rect> = placement.panes.iter().map(|(_, card)| card.rect).collect();
        assert_eq!(
            rects,
            [
                rect(0.0, 0.0, 200.0, 210.0),
                rect(210.0, 0.0, 200.0, 100.0),
                rect(210.0, 110.0, 200.0, 100.0),
            ]
        );
        // Inside a 200 × 100 card's inset of 5: 19 columns and 4 rows.
        let card = placement.card(PaneId(2)).unwrap();
        assert_eq!((card.cols, card.rows, card.origin), (19, 4, [215.0, 5.0]));
    }

    #[test]
    fn pixels_left_over_go_to_the_first_parts() {
        let layout = split(Axis::LeftToRight, vec![pane(1), pane(2), pane(3)]);
        let widths = |area: &Area| -> Vec<f32> {
            Placement::new(&layout, area)
                .panes
                .iter()
                .map(|(_, card)| card.rect.width)
                .collect()
        };
        // 410 less two gaps is 390: 130 each.
        assert_eq!(widths(&area()), [130.0, 130.0, 130.0]);
        let mut wider = area();
        wider.rect.width = 412.0;
        assert_eq!(widths(&wider), [131.0, 131.0, 130.0]);
    }

    #[test]
    fn neighbours_are_found_by_direction_and_cards_by_point() {
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
}
