//! The window's own frame: the top bar, with the workspaces and the
//! window's buttons; the bottom bar, with the tabs and a note; the cards
//! the panes sit on; and the palette, in the middle, while a command or a
//! search is typed. Where each goes, what a point in the window is over,
//! for clicks and the mouse cursor, and how to draw it all.
//!
//! The window draws its own frame instead of the desktop's: dragging the
//! top bar moves the window, and its edges resize it. The buttons go
//! where the desktop's setting puts them.

use clackatoo_protocol::{TabId, WorkspaceId};
use winit::window::ResizeDirection;

use crate::desktop::{Button, ButtonLayout};
use crate::layout::{Area, Rect};
use crate::render::{Fill, Ink, Paint, Text, TextFont};

/// The sizes everything else is worked out from, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Metrics {
    /// A terminal cell's width and height.
    pub(crate) cell: [f32; 2],
    /// A line of interface text's height.
    pub(crate) ui_line: f32,
    /// The unit of space between things.
    pub(crate) padding: f32,
}

impl Metrics {
    /// The height of a name in a bar, with room above and below its text.
    fn pill_height(self) -> f32 {
        (self.ui_line + 2.0 * self.padding).round()
    }

    fn top_height(self) -> f32 {
        self.pill_height() + 3.0 * self.padding
    }

    fn bottom_height(self) -> f32 {
        self.pill_height() + 2.0 * self.padding
    }

    /// The space between cards, and between them and the window's edges.
    fn gap(self) -> f32 {
        2.0 * self.padding
    }

    /// How far in from a card's edges its cells start.
    fn inset(self) -> f32 {
        2.0 * self.padding
    }
}

/// Where the parts of the window go, for a window of one size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Geometry {
    pub(crate) size: [f32; 2],
    pub(crate) metrics: Metrics,
    pub(crate) top: Rect,
    pub(crate) bottom: Rect,
    /// Where the cards go.
    pub(crate) area: Area,
    /// How round the cards' corners are.
    pub(crate) radius: f32,
    /// How round the window's corners are, where it can be see-through and
    /// isn't maximised.
    pub(crate) window_radius: f32,
    /// How thick their edges are.
    pub(crate) edge: f32,
}

impl Geometry {
    /// For a window `size` pixels.
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn new(size: [u32; 2], metrics: Metrics) -> Self {
        let [width, height] = [size[0] as f32, size[1] as f32];
        let top = Rect {
            x: 0.0,
            y: 0.0,
            width,
            height: metrics.top_height(),
        };
        let bottom = Rect {
            x: 0.0,
            y: height - metrics.bottom_height(),
            width,
            height: metrics.bottom_height(),
        };
        let gap = metrics.gap();
        let area = Area {
            rect: Rect {
                x: gap,
                y: top.height,
                width: (width - 2.0 * gap).max(1.0),
                height: (bottom.y - top.height).max(1.0),
            },
            cell: metrics.cell,
            gap,
            inset: metrics.inset(),
        };
        Self {
            size: [width, height],
            metrics,
            top,
            bottom,
            area,
            radius: (2.5 * metrics.padding).round(),
            window_radius: (3.0 * metrics.padding).round(),
            edge: (metrics.padding / 4.0).round().max(1.0),
        }
    }

    /// The window size whose one card holds `cols` × `rows` cells, in the
    /// units `metrics` is in.
    pub(crate) fn window_size(cols: u16, rows: u16, metrics: Metrics) -> [f32; 2] {
        let [cell_width, cell_height] = metrics.cell;
        let inset = 2.0 * metrics.inset();
        [
            f32::from(cols) * cell_width + inset + 2.0 * metrics.gap(),
            f32::from(rows) * cell_height + inset + metrics.top_height() + metrics.bottom_height(),
        ]
    }
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

/// What the bottom bar and the palette show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Something being typed, such as a command or a search, in the
    /// palette.
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
    /// Where the note starts, if there is one.
    pub(crate) note_x: f32,
    /// The palette, while something's being typed.
    pub(crate) palette: Option<Rect>,
    /// Whether to draw the bottom bar: notices take its place.
    pub(crate) bottom_shown: bool,
}

/// What goes in the frame, besides where.
#[derive(Debug)]
pub(crate) struct Contents<'a> {
    pub(crate) workspaces: Vec<(WorkspaceId, String, bool)>,
    pub(crate) tabs: Vec<(TabId, String, bool)>,
    pub(crate) status: Status,
    pub(crate) buttons: &'a ButtonLayout,
    /// Whether to draw the bottom bar: notices take its place.
    pub(crate) bottom_shown: bool,
}

impl Chrome {
    /// Lays out the frame: the buttons at the ends of the top bar where
    /// the desktop puts them, the workspaces from its left, and the tabs
    /// from the bottom bar's left. `measure` says how wide interface text
    /// is, bold or not. Names that don't fit are left out.
    pub(crate) fn new(
        geometry: Geometry,
        contents: Contents<'_>,
        measure: &mut dyn FnMut(&str, bool) -> f32,
    ) -> Self {
        let metrics = geometry.metrics;
        let padding = metrics.padding;
        let top = geometry.top;
        let pill_height = metrics.pill_height();
        let pill_y = top.y + (top.height - pill_height) / 2.0;
        let diameter = (pill_height * 0.8).round();
        let button_y = (top.y + (top.height - diameter) / 2.0).round();
        let spacing = 2.0 * padding;
        let button = |x: f32| Rect {
            x,
            y: button_y,
            width: diameter,
            height: diameter,
        };
        let mut buttons = Vec::new();
        let mut x = 3.0 * padding;
        for &kind in &contents.buttons.left {
            buttons.push((kind, button(x)));
            x += diameter + spacing;
        }
        let workspaces_start = if contents.buttons.left.is_empty() {
            geometry.area.rect.x
        } else {
            x + padding
        };
        let mut x = top.width - 3.0 * padding;
        for &kind in contents.buttons.right.iter().rev() {
            x -= diameter;
            buttons.push((kind, button(x)));
            x -= spacing;
        }
        let workspaces_end = x - padding;

        let row = Row {
            height: pill_height,
            padding,
        };
        let workspaces = row.lay_out(
            contents.workspaces,
            workspaces_start..workspaces_end,
            pill_y,
            measure,
        );
        let status = contents.status;
        let bottom = geometry.bottom;
        let note_width = match &status {
            Status::Tabs { note: Some(note) } => measure(note, false),
            _ => 0.0,
        };
        let card_right = geometry.area.rect.x + geometry.area.rect.width;
        let note_x = (card_right - padding - note_width).round();
        let tabs = match &status {
            Status::Tabs { .. } => row.lay_out(
                contents.tabs,
                geometry.area.rect.x..note_x - padding,
                bottom.y + (bottom.height - pill_height) / 2.0,
                measure,
            ),
            Status::Typing(_) => Vec::new(),
        };
        let palette = matches!(status, Status::Typing(_)).then(|| {
            let [width, height] = geometry.size;
            let palette_width = (width * 0.6)
                .min(90.0 * metrics.cell[0])
                .min(width - 2.0 * spacing);
            let palette_height = (metrics.cell[1] + 4.0 * padding).round();
            Rect {
                x: ((width - palette_width) / 2.0).round(),
                y: ((height - palette_height) / 2.0).round(),
                width: palette_width.round(),
                height: palette_height,
            }
        });
        Self {
            geometry,
            workspaces,
            buttons,
            tabs,
            status,
            note_x,
            palette,
            bottom_shown: contents.bottom_shown,
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
        if self.bottom_shown {
            if let Some(item) = self.tabs.iter().find(|item| item.rect.contains(point)) {
                return Hit::Tab(item.id);
            }
            if self.geometry.bottom.contains(point) {
                return Hit::Nothing;
            }
        }
        if self.geometry.area.rect.contains(point) {
            Hit::Cards
        } else {
            Hit::Nothing
        }
    }

    /// Which way the window resizes from `point`, if it's near an edge:
    /// within a padding and a half of it, or for a corner, three times
    /// that along either edge.
    fn edge(&self, [x, y]: [f32; 2]) -> Option<ResizeDirection> {
        let [width, height] = self.geometry.size;
        let band = (self.geometry.metrics.padding * 1.5).max(4.0);
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

    /// The frame's rectangles and text, with `hover` under the mouse: those
    /// of the bars, under the cards, and those of the palette, over them.
    pub(crate) fn paint(&self, hover: Hit) -> Painted<'_> {
        let metrics = self.geometry.metrics;
        let mut painted = Painted::default();
        for item in &self.workspaces {
            paint_item(
                item,
                hover == Hit::Workspace(item.id),
                metrics,
                &mut painted,
            );
        }
        for &(button, rect) in &self.buttons {
            self.paint_button(button, rect, hover == Hit::Button(button), &mut painted);
        }
        if self.bottom_shown {
            for item in &self.tabs {
                paint_item(item, hover == Hit::Tab(item.id), metrics, &mut painted);
            }
            if let Status::Tabs { note: Some(note) } = &self.status {
                let bottom = self.geometry.bottom;
                painted.texts.push(Text {
                    origin: [
                        self.note_x,
                        bottom.y + (bottom.height - metrics.ui_line) / 2.0,
                    ],
                    content: note,
                    ink: Ink::Muted,
                    font: TextFont::Ui,
                });
            }
        }
        if let (Some(rect), Status::Typing(text)) = (self.palette, &self.status) {
            // Layers of shadow, wider and fainter outwards, so it reads
            // as soft.
            for spread in [3.0, 2.0, 1.0] {
                let grow = spread * metrics.padding;
                painted.over.push(Fill::rounded(
                    [
                        rect.x - grow,
                        rect.y - grow + metrics.padding,
                        rect.width + 2.0 * grow,
                        rect.height + 2.0 * grow,
                    ],
                    Paint::Shadow,
                    self.geometry.radius + grow,
                ));
            }
            painted.over.push(
                Fill::rounded(rect.to_array(), Paint::Palette, self.geometry.radius)
                    .bordered(self.geometry.edge, Paint::Focus),
            );
            painted.over_texts.push(Text {
                origin: [
                    rect.x + 2.0 * metrics.padding,
                    rect.y + (rect.height - metrics.cell[1]) / 2.0,
                ],
                content: text,
                ink: Ink::Plain,
                font: TextFont::Mono,
            });
        }
        painted
    }

    /// A round button with its icon: a line for minimise, a square for
    /// maximise and a cross for close.
    fn paint_button(&self, button: Button, rect: Rect, hovered: bool, painted: &mut Painted<'_>) {
        let metrics = self.geometry.metrics;
        let stroke = self.geometry.edge;
        let paint = if hovered {
            Paint::Picked
        } else {
            Paint::Button
        };
        painted
            .fills
            .push(Fill::rounded(rect.to_array(), paint, rect.width / 2.0));
        let side = (rect.width * 0.36).round();
        let left = (rect.x + (rect.width - side) / 2.0).round();
        let top = (rect.y + (rect.height - side) / 2.0).round();
        match button {
            Button::Minimise => painted.fills.push(Fill::solid(
                [left, top + side - stroke, side, stroke],
                Paint::Ink,
            )),
            Button::Maximise => painted.fills.push(
                Fill::rounded([left, top, side, side], Paint::Clear, stroke)
                    .bordered(stroke, Paint::Ink),
            ),
            Button::Close => painted.texts.push(Text {
                origin: [
                    (rect.x + rect.width / 2.0 - metrics.ui_line * 0.27).round(),
                    (rect.y + (rect.height - metrics.ui_line) / 2.0).round(),
                ],
                content: "×",
                ink: Ink::Plain,
                font: TextFont::Ui,
            }),
        }
    }
}

/// What the frame draws: under the cards, and over them.
#[derive(Debug, Default)]
pub(crate) struct Painted<'a> {
    pub(crate) fills: Vec<Fill>,
    pub(crate) texts: Vec<Text<'a>>,
    /// Over the cards and their text, such as the palette.
    pub(crate) over: Vec<Fill>,
    pub(crate) over_texts: Vec<Text<'a>>,
}

/// Draws a name: picked out if it's the one shown, lit up if the mouse is
/// over it, and quieter otherwise.
fn paint_item<'a, T>(
    item: &'a Item<T>,
    hovered: bool,
    metrics: Metrics,
    painted: &mut Painted<'a>,
) {
    let radius = (1.5 * metrics.padding).round();
    let ink = if item.active {
        painted
            .fills
            .push(Fill::rounded(item.rect.to_array(), Paint::Picked, radius));
        Ink::Strong
    } else if hovered {
        painted
            .fills
            .push(Fill::rounded(item.rect.to_array(), Paint::Hover, radius));
        Ink::Plain
    } else {
        Ink::Muted
    };
    painted.texts.push(Text {
        origin: [
            item.rect.x + 3.0 * metrics.padding,
            (item.rect.y + (item.rect.height - metrics.ui_line) / 2.0).round(),
        ],
        content: &item.text,
        ink,
        font: TextFont::Ui,
    });
}

/// A row of names in a bar.
#[derive(Clone, Copy, Debug)]
struct Row {
    height: f32,
    padding: f32,
}

impl Row {
    /// Places `items` side by side across `span`, a padding apart, at `y`,
    /// each as wide as its text and three paddings either side. Those that
    /// would pass the span's end are left out.
    fn lay_out<T>(
        self,
        items: Vec<(T, String, bool)>,
        span: std::ops::Range<f32>,
        y: f32,
        measure: &mut dyn FnMut(&str, bool) -> f32,
    ) -> Vec<Item<T>> {
        let mut x = span.start;
        let mut placed = Vec::new();
        for (id, text, active) in items {
            let width = (measure(&text, active) + 6.0 * self.padding).round();
            if x + width > span.end {
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
                    height: self.height,
                },
            });
            x += width + self.padding;
        }
        placed
    }
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
    /// The space the cards share.
    Cards,
    Nothing,
}

#[cfg(test)]
// Every position here is a whole number of pixels, so exact.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// Cells 10 × 20, interface text 16 high, and a padding of 4: names in
    /// the bars are 24 high, the top bar 36 and the bottom bar 32.
    const METRICS: Metrics = Metrics {
        cell: [10.0, 20.0],
        ui_line: 16.0,
        padding: 4.0,
    };

    /// Every character 8 wide.
    fn measure(text: &str, _bold: bool) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let width = text.chars().count() as f32 * 8.0;
        width
    }

    /// An 800 × 600 window.
    fn chrome(status: Status, buttons: &ButtonLayout) -> Chrome {
        Chrome::new(
            Geometry::new([800, 600], METRICS),
            Contents {
                workspaces: vec![
                    (WorkspaceId(1), "clackatoo".into(), true),
                    (WorkspaceId(2), "peios".into(), false),
                ],
                tabs: vec![
                    (TabId(1), "1 zsh".into(), true),
                    (TabId(2), "2 htop".into(), false),
                ],
                status,
                buttons,
                bottom_shown: true,
            },
            &mut measure,
        )
    }

    #[test]
    fn the_cards_fill_the_space_between_the_bars() {
        let geometry = Geometry::new([800, 600], METRICS);
        assert_eq!((geometry.top.height, geometry.bottom.y), (36.0, 568.0));
        assert_eq!(
            geometry.area.rect,
            Rect {
                x: 8.0,
                y: 36.0,
                width: 784.0,
                height: 532.0
            }
        );
        // 100 × 30 cells, inset 8 in a card, with gaps of 8 either side.
        assert_eq!(Geometry::window_size(100, 30, METRICS), [1032.0, 684.0]);
    }

    #[test]
    fn points_hit_what_they_are_over() {
        let chrome = chrome(
            Status::Tabs {
                note: Some("NORMAL".into()),
            },
            &ButtonLayout::default(),
        );
        // clackatoo's pill starts at the cards' left edge, 8, and is 9
        // characters and 24 of padding wide: 96.
        assert_eq!(chrome.workspaces[0].rect.x, 8.0);
        assert_eq!(chrome.workspaces[0].rect.width, 96.0);
        assert_eq!(
            chrome.hit([20.0, 18.0], false),
            Hit::Workspace(WorkspaceId(1))
        );
        assert_eq!(
            chrome.hit([120.0, 18.0], false),
            Hit::Workspace(WorkspaceId(2))
        );
        assert_eq!(chrome.hit([400.0, 18.0], false), Hit::TopBar);
        // The buttons are on the right, close last.
        let close = chrome
            .buttons
            .iter()
            .find(|(button, _)| *button == Button::Close)
            .unwrap()
            .1;
        assert_eq!(close.x + close.width, 788.0);
        assert_eq!(
            chrome.hit([close.x + 5.0, 18.0], false),
            Hit::Button(Button::Close)
        );
        assert_eq!(chrome.hit([20.0, 584.0], false), Hit::Tab(TabId(1)));
        assert_eq!(chrome.hit([400.0, 584.0], false), Hit::Nothing);
        assert_eq!(chrome.hit([100.0, 100.0], false), Hit::Cards);
    }

    #[test]
    fn buttons_on_the_left_push_the_workspaces_over() {
        let buttons = ButtonLayout {
            left: vec![Button::Close],
            right: Vec::new(),
        };
        let chrome = chrome(Status::Tabs { note: None }, &buttons);
        let close = chrome.buttons[0].1;
        assert_eq!(close.x, 12.0);
        assert!(chrome.workspaces[0].rect.x > close.x + close.width);
    }

    #[test]
    fn edges_and_corners_resize_unless_maximised() {
        let chrome = chrome(Status::Tabs { note: None }, &ButtonLayout::default());
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
    fn typing_opens_the_palette_in_the_middle() {
        let chrome = chrome(
            Status::Typing(":|Terminal".into()),
            &ButtonLayout::default(),
        );
        assert!(chrome.tabs.is_empty());
        let palette = chrome.palette.expect("a palette");
        assert_eq!(palette.x + palette.width / 2.0, 400.0);
        assert_eq!(palette.y + palette.height / 2.0, 300.0);
    }
}
