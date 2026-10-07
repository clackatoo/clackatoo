//! The window's own frame: the top bar, with the workspaces and the
//! window's buttons, and the status line along the bottom, with the tabs,
//! the mode and the command line. Where each part goes, and what a point in
//! the window is over, for clicks and the mouse cursor.
//!
//! The window draws its own frame instead of the desktop's, so it holds no
//! title bar of its own: dragging the top bar moves the window, and its
//! edges resize it.

use clackatoo_protocol::{Size, TabId, WorkspaceId};
use winit::window::ResizeDirection;

use crate::layout::{Cells, Divider};
use crate::render::{Fill, Ink, Paint, Text};

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
}

/// Where the parts of the window go, in pixels, for a window of one size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Geometry {
    pub(crate) size: [f32; 2],
    pub(crate) padding: f32,
    pub(crate) cell: [f32; 2],
    /// The top bar, across the top.
    pub(crate) top: Rect,
    /// The status line, across the bottom.
    pub(crate) status: Rect,
    /// The cells between the bars, for panes.
    pub(crate) panes: Cells,
    /// Where the panes' cells start.
    pub(crate) panes_origin: [f32; 2],
    /// Every cell below the top bar, for notices, which need no status line.
    pub(crate) grid: Size,
}

impl Geometry {
    /// For a window `size` pixels, with cells `cell` pixels and `padding`
    /// pixels around things.
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn new(size: [u32; 2], cell: [u32; 2], padding: u32) -> Option<Self> {
        let cells =
            |pixels: u32, cell: u32| u16::try_from((pixels / cell).max(1)).unwrap_or(u16::MAX);
        let [width, height] = size;
        let [cell_width, cell_height] = cell;
        // A row of text with room above and below, as title bars have.
        let top_height = cell_height + 4 * padding;
        let status_height = cell_height + padding;
        let below_top = top_height + padding;
        let grid = Size::new(
            cells(width.saturating_sub(2 * padding), cell_width),
            cells(height.saturating_sub(below_top + padding), cell_height),
        )
        .ok()?;
        let panes = Cells {
            col: 0,
            row: 0,
            cols: grid.cols(),
            rows: cells(
                height.saturating_sub(below_top + status_height + padding),
                cell_height,
            ),
        };
        let (width, height) = (width as f32, height as f32);
        Some(Self {
            size: [width, height],
            padding: padding as f32,
            cell: [cell_width as f32, cell_height as f32],
            top: Rect {
                x: 0.0,
                y: 0.0,
                width,
                height: top_height as f32,
            },
            status: Rect {
                x: 0.0,
                y: height - status_height as f32,
                width,
                height: status_height as f32,
            },
            panes,
            panes_origin: [padding as f32, below_top as f32],
            grid,
        })
    }

    /// The window size that holds `cols` × `rows` cells of panes, in the
    /// units `cell` and `padding` are in.
    pub(crate) fn window_size(cols: u32, rows: u32, cell: [f64; 2], padding: f64) -> [f64; 2] {
        let [cell_width, cell_height] = cell;
        let top = cell_height + 4.0 * padding;
        let status = cell_height + padding;
        [
            f64::from(cols) * cell_width + 2.0 * padding,
            f64::from(rows) * cell_height + top + status + 2.0 * padding,
        ]
    }

    pub(crate) fn origin(&self, cells: Cells) -> [f32; 2] {
        [
            self.panes_origin[0] + f32::from(cells.col) * self.cell[0],
            self.panes_origin[1] + f32::from(cells.row) * self.cell[1],
        ]
    }

    pub(crate) fn pixels(&self, cells: Cells) -> [f32; 2] {
        [
            f32::from(cells.cols) * self.cell[0],
            f32::from(cells.rows) * self.cell[1],
        ]
    }

    /// Where notices go: every cell below the top bar.
    pub(crate) fn grid_origin(&self) -> [f32; 2] {
        self.panes_origin
    }

    pub(crate) fn grid_pixels(&self) -> [f32; 2] {
        [
            f32::from(self.grid.cols()) * self.cell[0],
            f32::from(self.grid.rows()) * self.cell[1],
        ]
    }

    /// How wide `text` is as a tag: its characters, and a cell's space
    /// either side.
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn tag_width(&self, text: &str) -> f32 {
        (text.chars().count() as f32 + 2.0) * self.cell[0]
    }

    /// The cell of the pane area at `point`, if it's over one.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub(crate) fn cell_at(&self, [x, y]: [f32; 2]) -> Option<(u16, u16)> {
        let col = ((x - self.panes_origin[0]) / self.cell[0]).floor();
        let row = ((y - self.panes_origin[1]) / self.cell[1]).floor();
        let inside = col >= 0.0
            && row >= 0.0
            && col < f32::from(self.panes.cols)
            && row < f32::from(self.panes.rows);
        inside.then_some((col as u16, row as u16))
    }
}

/// One of the window's buttons, at the top bar's right end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Button {
    Minimise,
    Maximise,
    Close,
}

/// A name in a bar, and where it is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item<T> {
    pub(crate) id: T,
    pub(crate) text: String,
    /// The one shown, drawn picked out.
    pub(crate) active: bool,
    pub(crate) rect: Rect,
}

/// What the status line shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Something being typed, such as a command or a search, across the
    /// whole line.
    Typing(String),
    /// The tabs, and on the right a note: the mode, how far the view is
    /// scrolled, or a message.
    Tabs { note: Option<String> },
}

/// Where everything in the window's frame goes.
#[derive(Clone, Debug)]
pub(crate) struct Chrome {
    pub(crate) geometry: Geometry,
    pub(crate) workspaces: Vec<Item<WorkspaceId>>,
    pub(crate) buttons: Vec<(Button, Rect)>,
    pub(crate) tabs: Vec<Item<TabId>>,
    pub(crate) status: Status,
    /// Where the status line's note starts.
    pub(crate) note_x: f32,
    /// Whether to draw the status line: notices take its place.
    pub(crate) status_shown: bool,
}

impl Chrome {
    /// Lays out the frame: the workspaces from the top bar's left, the
    /// buttons at its right, and the tabs from the status line's left.
    /// Names that don't fit are left out.
    pub(crate) fn new(
        geometry: Geometry,
        workspaces: Vec<(WorkspaceId, String, bool)>,
        tabs: Vec<(TabId, String, bool)>,
        status: Status,
        status_shown: bool,
    ) -> Self {
        let padding = geometry.padding;
        let top = geometry.top;
        let button_width = top.height * 1.25;
        let mut buttons = Vec::new();
        for (index, button) in [Button::Close, Button::Maximise, Button::Minimise]
            .into_iter()
            .enumerate()
        {
            #[allow(clippy::cast_precision_loss)]
            let x = top.width - button_width * (index as f32 + 1.0);
            buttons.push((
                button,
                Rect {
                    x,
                    y: top.y,
                    width: button_width,
                    height: top.height,
                },
            ));
        }
        let buttons_start = top.width - 3.0 * button_width;

        let tag_height = geometry.cell[1] + padding;
        let workspaces = lay_out(
            &geometry,
            workspaces,
            top.y + (top.height - tag_height) / 2.0,
            tag_height,
            buttons_start - padding,
        );
        let note_width = match &status {
            Status::Tabs { note: Some(note) } => geometry.tag_width(note),
            _ => 0.0,
        };
        let note_x = geometry.status.width - 2.0 * padding - note_width;
        let tabs = match &status {
            Status::Tabs { .. } => lay_out(
                &geometry,
                tabs,
                geometry.status.y,
                geometry.status.height,
                note_x - padding,
            ),
            Status::Typing(_) => Vec::new(),
        };
        Self {
            geometry,
            workspaces,
            buttons,
            tabs,
            status,
            note_x,
            status_shown,
        }
    }

    /// What `point` is over. `maximised` windows can't be resized from
    /// their edges.
    pub(crate) fn hit(&self, point: [f32; 2], maximised: bool) -> Hit {
        if !maximised && let Some(direction) = self.edge(point) {
            return Hit::Resize(direction);
        }
        if let Some(&(button, _)) = self.buttons.iter().find(|(_, rect)| rect.contains(point)) {
            return Hit::Button(button);
        }
        if let Some(item) = self
            .workspaces
            .iter()
            .find(|item| item.rect.contains(point))
        {
            return Hit::Workspace(item.id);
        }
        if self.geometry.top.contains(point) {
            return Hit::TopBar;
        }
        if self.status_shown {
            if let Some(item) = self.tabs.iter().find(|item| item.rect.contains(point)) {
                return Hit::Tab(item.id);
            }
            if self.geometry.status.contains(point) {
                return Hit::Nothing;
            }
        }
        match self.geometry.cell_at(point) {
            Some((col, row)) => Hit::Cell { col, row },
            None => Hit::Nothing,
        }
    }

    /// Which way the window resizes from `point`, if it's near an edge:
    /// within a padding and a half of it, or for a corner, three times
    /// that along either edge.
    fn edge(&self, [x, y]: [f32; 2]) -> Option<ResizeDirection> {
        let [width, height] = self.geometry.size;
        let band = (self.geometry.padding * 1.5).max(4.0);
        let corner = 3.0 * band;
        let (left, right) = (x < band, x >= width - band);
        let (top, bottom) = (y < band, y >= height - band);
        let (near_left, near_right) = (x < corner, x >= width - corner);
        let (near_top, near_bottom) = (y < corner, y >= height - corner);
        Some(match () {
            () if (top && near_left) || (left && near_top) => ResizeDirection::NorthWest,
            () if (top && near_right) || (right && near_top) => ResizeDirection::NorthEast,
            () if (bottom && near_left) || (left && near_bottom) => ResizeDirection::SouthWest,
            () if (bottom && near_right) || (right && near_bottom) => ResizeDirection::SouthEast,
            () if top => ResizeDirection::North,
            () if bottom => ResizeDirection::South,
            () if left => ResizeDirection::West,
            () if right => ResizeDirection::East,
            () => return None,
        })
    }
}

impl Chrome {
    /// The frame's rectangles and text, with `hover` under the mouse.
    pub(crate) fn paint(&self, hover: Hit) -> (Vec<Fill>, Vec<Text<'_>>) {
        let geometry = &self.geometry;
        let [cell_width, cell_height] = geometry.cell;
        let mut fills = vec![Fill {
            rect: geometry.top.to_array(),
            paint: Paint::Bar,
        }];
        let mut placed = Vec::new();
        for item in &self.workspaces {
            let hovered = hover == Hit::Workspace(item.id);
            let (origin, ink) = paint_tag(geometry, item.rect, item.active, hovered, &mut fills);
            placed.push(Text {
                origin,
                content: &item.text,
                ink,
            });
        }
        let stroke = (geometry.padding / 4.0).round().max(1.0);
        for &(button, rect) in &self.buttons {
            if hover == Hit::Button(button) {
                fills.push(Fill {
                    rect: rect.to_array(),
                    paint: if button == Button::Close {
                        Paint::Danger
                    } else {
                        Paint::Hover
                    },
                });
            }
            let centre = [rect.x + rect.width / 2.0, rect.y + rect.height / 2.0];
            let side = (cell_height * 0.45).round();
            let left = (centre[0] - side / 2.0).round();
            let top = (centre[1] - side / 2.0).round();
            let line = |x: f32, y: f32, width: f32, height: f32| Fill {
                rect: [x, y, width, height],
                paint: Paint::Ink,
            };
            match button {
                Button::Minimise => fills.push(line(left, top + side - stroke, side, stroke)),
                Button::Maximise => fills.extend([
                    line(left, top, side, stroke),
                    line(left, top + side - stroke, side, stroke),
                    line(left, top, stroke, side),
                    line(left + side - stroke, top, stroke, side),
                ]),
                Button::Close => placed.push(Text {
                    origin: [centre[0] - cell_width / 2.0, centre[1] - cell_height / 2.0],
                    content: "×",
                    ink: Ink::Plain,
                }),
            }
        }
        if self.status_shown {
            let status = geometry.status;
            fills.push(Fill {
                rect: status.to_array(),
                paint: Paint::Bar,
            });
            let text_top = status.y + (status.height - cell_height) / 2.0;
            match &self.status {
                Status::Typing(text) => placed.push(Text {
                    origin: [2.0 * geometry.padding, text_top],
                    content: text,
                    ink: Ink::Plain,
                }),
                Status::Tabs { note } => {
                    for item in &self.tabs {
                        let hovered = hover == Hit::Tab(item.id);
                        let (origin, ink) =
                            paint_tag(geometry, item.rect, item.active, hovered, &mut fills);
                        placed.push(Text {
                            origin,
                            content: &item.text,
                            ink,
                        });
                    }
                    if let Some(note) = note {
                        placed.push(Text {
                            origin: [self.note_x + cell_width, text_top],
                            content: note,
                            ink: Ink::Plain,
                        });
                    }
                }
            }
        }
        (fills, placed)
    }
}

/// A divider's line, as a fill: thin, down or across the middle of its
/// cells.
pub(crate) fn divider_fill(geometry: &Geometry, divider: &Divider) -> Fill {
    let [left, top] = geometry.origin(divider.cells);
    let [width, height] = geometry.pixels(divider.cells);
    let thickness = (geometry.padding / 4.0).round().max(1.0);
    let rect = match divider.axis {
        clackatoo_protocol::Axis::LeftToRight => [
            (left + (width - thickness) / 2.0).round(),
            top,
            thickness,
            height,
        ],
        clackatoo_protocol::Axis::TopToBottom => [
            left,
            (top + (height - thickness) / 2.0).round(),
            width,
            thickness,
        ],
    };
    Fill {
        rect,
        paint: Paint::Divider,
    }
}

/// Places `items` side by side from the left, a padding apart, in a row
/// at `y`, `height` high. Those that would pass `end` are left out.
fn lay_out<T>(
    geometry: &Geometry,
    items: Vec<(T, String, bool)>,
    y: f32,
    height: f32,
    end: f32,
) -> Vec<Item<T>> {
    let padding = geometry.padding;
    let mut x = 2.0 * padding;
    let mut placed = Vec::new();
    for (id, text, active) in items {
        let width = geometry.tag_width(&text);
        if x + width > end {
            break;
        }
        placed.push(Item {
            id,
            text,
            active,
            rect: Rect {
                x,
                y,
                width,
                height,
            },
        });
        x += width + padding;
    }
    placed
}

/// Fills in a name's background: the accent colour if it's `active`, or a
/// highlight if it's `hovered`. Returns where its text goes, and how.
fn paint_tag(
    geometry: &Geometry,
    rect: Rect,
    active: bool,
    hovered: bool,
    fills: &mut Vec<Fill>,
) -> ([f32; 2], Ink) {
    let [cell_width, cell_height] = geometry.cell;
    let ink = if active {
        fills.push(Fill {
            rect: rect.to_array(),
            paint: Paint::Accent,
        });
        Ink::OnAccent
    } else if hovered {
        fills.push(Fill {
            rect: rect.to_array(),
            paint: Paint::Hover,
        });
        Ink::Plain
    } else {
        Ink::Muted
    };
    (
        [
            rect.x + cell_width,
            rect.y + (rect.height - cell_height) / 2.0,
        ],
        ink,
    )
}

/// What a point in the window is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hit {
    /// An edge or corner, which resizes the window this way.
    Resize(ResizeDirection),
    Button(Button),
    Workspace(WorkspaceId),
    /// The top bar, away from its names and buttons, which moves the
    /// window.
    TopBar,
    Tab(TabId),
    /// A cell of the pane area.
    Cell {
        col: u16,
        row: u16,
    },
    Nothing,
}

#[cfg(test)]
// Every position here is a whole number of pixels, so exact.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// A window 800 × 600, with cells 10 × 20 and a padding of 4: a top bar
    /// 36 high, a status line 24 high, and panes of 79 × 26 cells between.
    fn chrome(status: Status) -> Chrome {
        let geometry = Geometry::new([800, 600], [10, 20], 4).unwrap();
        Chrome::new(
            geometry,
            vec![
                (WorkspaceId(1), "clackatoo".into(), true),
                (WorkspaceId(2), "peios".into(), false),
            ],
            vec![
                (TabId(1), "1 zsh".into(), true),
                (TabId(2), "2 htop".into(), false),
            ],
            status,
            true,
        )
    }

    #[test]
    fn the_panes_fill_the_space_between_the_bars() {
        let geometry = chrome(Status::Tabs { note: None }).geometry;
        assert_eq!(geometry.top.height, 36.0);
        assert_eq!(geometry.status.y, 576.0);
        assert_eq!((geometry.panes.cols, geometry.panes.rows), (79, 26));
        assert_eq!(geometry.panes_origin, [4.0, 40.0]);
        let [width, height] = Geometry::window_size(79, 26, [10.0, 20.0], 4.0);
        assert_eq!(
            (width, height),
            (798.0, 588.0),
            "room for exactly those cells"
        );
    }

    #[test]
    fn points_hit_what_they_are_over() {
        let chrome = chrome(Status::Tabs {
            note: Some("NORMAL".into()),
        });
        // clackatoo's tag starts at 8 and is 11 cells wide; peios's follows.
        assert_eq!(
            chrome.hit([20.0, 18.0], false),
            Hit::Workspace(WorkspaceId(1))
        );
        assert_eq!(
            chrome.hit([130.0, 18.0], false),
            Hit::Workspace(WorkspaceId(2))
        );
        assert_eq!(chrome.hit([400.0, 18.0], false), Hit::TopBar);
        assert_eq!(chrome.hit([790.0, 18.0], false), Hit::Button(Button::Close));
        assert_eq!(chrome.hit([20.0, 590.0], false), Hit::Tab(TabId(1)));
        assert_eq!(chrome.hit([400.0, 590.0], false), Hit::Nothing);
        assert_eq!(
            chrome.hit([14.0, 60.0], false),
            Hit::Cell { col: 1, row: 1 }
        );
    }

    #[test]
    fn edges_and_corners_resize_unless_maximised() {
        let chrome = chrome(Status::Tabs { note: None });
        assert_eq!(
            chrome.hit([2.0, 300.0], false),
            Hit::Resize(ResizeDirection::West)
        );
        assert_eq!(
            chrome.hit([798.0, 300.0], false),
            Hit::Resize(ResizeDirection::East)
        );
        assert_eq!(
            chrome.hit([400.0, 1.0], false),
            Hit::Resize(ResizeDirection::North)
        );
        assert_eq!(
            chrome.hit([2.0, 10.0], false),
            Hit::Resize(ResizeDirection::NorthWest)
        );
        assert_eq!(
            chrome.hit([795.0, 598.0], false),
            Hit::Resize(ResizeDirection::SouthEast)
        );
        assert_eq!(chrome.hit([400.0, 1.0], true), Hit::TopBar);
    }

    #[test]
    fn typing_takes_the_whole_status_line() {
        let chrome = chrome(Status::Typing(":|Terminal".into()));
        assert!(chrome.tabs.is_empty());
        assert_eq!(chrome.hit([20.0, 590.0], false), Hit::Nothing);
    }
}
