//! The window's own frame: the top bar, with the workspaces, any alert,
//! and the window's buttons; the bottom bar, with the mode, the tabs and
//! notes; the panes' backgrounds, side by side with lines between; and the
//! palette, in the middle, while a command or a search is typed. Where each
//! goes, what a point in the window is over, for clicks and the mouse
//! cursor, and how to draw it all.
//!
//! The window draws its own frame instead of the desktop's: dragging the
//! top bar moves the window, and its edges resize it. The buttons go
//! where the desktop's setting puts them.

use clackatoo_protocol::{TabId, WorkspaceId};
use winit::window::ResizeDirection;

use clackatoo_protocol::PaneId;

use crate::desktop::{Button, ButtonLayout};
use crate::editor::PanelView;
use crate::finder::{self, view::FinderView};
use crate::layout::{Area, Placement, Rect};
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

    /// A line of the terminal's text, and a little room.
    fn bottom_height(self) -> f32 {
        (self.cell[1] + 1.5 * self.padding).round()
    }

    /// How thick lines are: those between panes, and edges.
    fn line(self) -> f32 {
        (self.padding / 4.0).round().max(1.0)
    }

    /// How far in from a pane's edges its cells start.
    fn inset(self) -> f32 {
        3.0 * self.padding
    }

    /// The diameter of the dot by a workspace's name.
    fn dot(self) -> f32 {
        (self.ui_line * 0.4).round()
    }
}

/// Where the parts of the window go, for a window of one size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Geometry {
    pub(crate) size: [f32; 2],
    pub(crate) metrics: Metrics,
    pub(crate) top: Rect,
    pub(crate) bottom: Rect,
    /// Where the panes go, between the lines under the top bar and over
    /// the bottom one.
    pub(crate) area: Area,
    /// How round the corners of the palette and menus are.
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
        let line = metrics.line();
        let area = Area {
            rect: Rect {
                x: 0.0,
                y: top.height + line,
                width,
                height: (bottom.y - top.height - 2.0 * line).max(1.0),
            },
            cell: metrics.cell,
            gap: line,
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
            edge: line,
        }
    }

    /// The window size whose one pane holds `cols` × `rows` cells, in the
    /// units `metrics` is in.
    pub(crate) fn window_size(cols: u16, rows: u16, metrics: Metrics) -> [f32; 2] {
        let [cell_width, cell_height] = metrics.cell;
        let inset = 2.0 * metrics.inset();
        [
            f32::from(cols) * cell_width + inset,
            f32::from(rows) * cell_height
                + inset
                + metrics.top_height()
                + metrics.bottom_height()
                + 2.0 * metrics.line(),
        ]
    }
}

/// The mode the focused pane is in, named at the bottom bar's left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeName {
    Insert,
    Normal,
    Visual,
    VisualLine,
}

impl ModeName {
    fn text(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Normal => "NORMAL",
            Self::Visual => "VISUAL",
            Self::VisualLine => "V-LINE",
        }
    }

    /// Which of the theme's mode colours it's drawn on.
    pub(crate) fn colour(self) -> usize {
        match self {
            Self::Insert => 0,
            Self::Normal => 1,
            Self::Visual | Self::VisualLine => 2,
        }
    }

    /// A reminder of the key that does most from here.
    fn hint(self) -> &'static str {
        match self {
            Self::Insert => "^; normal mode",
            Self::Normal | Self::Visual | Self::VisualLine => ": command",
        }
    }
}

/// A name in a bar, and where it is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item<T> {
    pub(crate) id: T,
    pub(crate) text: String,
    /// Said more quietly after the name, such as a workspace's tabs, and
    /// where it starts.
    pub(crate) detail: Option<(String, f32)>,
    /// The one shown, drawn picked out.
    pub(crate) active: bool,
    pub(crate) rect: Rect,
}

/// What the bottom bar and the palette show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Something being typed, such as a command or a search, in the
    /// palette, perhaps after a label saying what it's for, and with
    /// choices listed under it, one of them picked.
    Typing {
        label: Option<String>,
        text: String,
        choices: Vec<(String, bool)>,
    },
    /// The focused pane's mode, the tabs, and on the right a note (how far
    /// the view is scrolled, or a message), how many panes the tab has,
    /// and a hint.
    Tabs {
        mode: Option<ModeName>,
        note: Option<String>,
        panes: usize,
    },
}

impl Status {
    /// The tabs, with nothing else to say.
    #[cfg(test)]
    pub(crate) fn tabs() -> Self {
        Self::Tabs {
            mode: None,
            note: None,
            panes: 1,
        }
    }
}

/// Where everything in the window's frame goes.
#[derive(Clone, Debug)]
pub(crate) struct Chrome {
    pub(crate) geometry: Geometry,
    pub(crate) workspaces: Vec<Item<WorkspaceId>>,
    pub(crate) buttons: Vec<(Button, Rect)>,
    pub(crate) tabs: Vec<Item<TabId>>,
    pub(crate) status: Status,
    /// The mode's name at the bottom bar's left, and where.
    pub(crate) mode: Option<(ModeName, Rect)>,
    /// Text at the bottom bar's right, and where each starts.
    pub(crate) notes: Vec<(String, f32)>,
    /// The palette, while something's being typed.
    pub(crate) palette: Option<Rect>,
    /// Where each of the palette's choices goes.
    pub(crate) choices: Vec<Rect>,
    /// Where the palette's text starts, after its label.
    pub(crate) palette_text_x: f32,
    /// The `+` after the workspaces, which makes one.
    pub(crate) new_workspace: Option<Rect>,
    /// The `+` after the tabs, which opens one.
    pub(crate) new_tab: Option<Rect>,
    /// A menu, if one is open: its box, and its items.
    pub(crate) menu: Option<(Rect, Vec<(String, Rect)>)>,
    /// Something gone wrong, such as the daemon being out of contact, and
    /// where it's shown: at the top bar's right, before the buttons.
    pub(crate) alert: Option<(String, Rect)>,
    /// Whether to draw the bottom bar: notices take its place.
    pub(crate) bottom_shown: bool,
    /// Whether the window's corners are rounded, which what's drawn in the
    /// bottom bar's corner must follow.
    pub(crate) rounded: bool,
    /// The finder, if it's open: where its parts go, and what it shows.
    pub(crate) finder: Option<(finder::view::Layout, FinderView)>,
}

/// What goes in the frame, besides where.
#[derive(Debug)]
pub(crate) struct Contents<'a> {
    /// Each workspace's name, how many tabs it has, and whether it's shown.
    pub(crate) workspaces: Vec<(WorkspaceId, String, usize, bool)>,
    pub(crate) tabs: Vec<(TabId, String, bool)>,
    pub(crate) status: Status,
    pub(crate) buttons: &'a ButtonLayout,
    /// Whether to draw the bottom bar: notices take its place.
    pub(crate) bottom_shown: bool,
    /// A menu to show: where it was asked for, and its items.
    pub(crate) menu: Option<([f32; 2], Vec<String>)>,
    /// Something gone wrong, to show in the top bar.
    pub(crate) alert: Option<String>,
    /// Whether the window's corners are rounded.
    pub(crate) rounded: bool,
    /// What the finder shows, if it's open, laid out as
    /// [`finder::view::Layout`] says for the window's size.
    pub(crate) finder: Option<FinderView>,
}

impl Chrome {
    /// Lays out the frame: the buttons at the ends of the top bar where
    /// the desktop puts them, the workspaces from its left, and the mode
    /// and the tabs from the bottom bar's left, with notes at its right.
    /// `measure` says how wide interface text is, bold or not. Names that
    /// don't fit are left out.
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
        let (buttons, workspaces_start, mut workspaces_end) =
            place_buttons(&geometry, contents.buttons);
        let alert = contents.alert.map(|text| {
            let width = (measure(&text, false) + 6.0 * padding).round();
            let rect = Rect {
                x: (workspaces_end - width).max(workspaces_start).round(),
                y: pill_y,
                width,
                height: pill_height,
            };
            workspaces_end = rect.x - padding;
            (text, rect)
        });

        // A workspace: its dot, its name, and quieter, how many tabs it has,
        // unless it's named with a number, as workspaces are by default:
        // "1 3" would read as nonsense.
        let dot = metrics.dot();
        let workspaces = lay_out(
            contents
                .workspaces
                .into_iter()
                .map(|(id, text, tabs, active)| {
                    let numbered = !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit());
                    (id, text, (!numbered).then(|| tabs.to_string()), active)
                }),
            workspaces_start..workspaces_end - pill_height - padding,
            padding,
            |text, detail, active| {
                let detail = detail.map_or(0.0, |detail| 1.5 * padding + measure(detail, false));
                (measure(text, active) + detail + dot + 8.0 * padding).round()
            },
            (pill_y, pill_height),
        );
        let plus = |after: f32, y: f32, side: f32| Rect {
            x: after + padding,
            y,
            width: side,
            height: side,
        };
        // Each tab count ends its pill, three paddings in.
        let mut workspaces = workspaces;
        for item in &mut workspaces {
            if let Some((detail, x)) = &mut item.detail {
                *x = (item.rect.x + item.rect.width - 3.0 * padding - measure(detail, false))
                    .round();
            }
        }
        let new_workspace = contents.bottom_shown.then(|| {
            let after = workspaces
                .last()
                .map_or(workspaces_start - padding, |item| {
                    item.rect.x + item.rect.width
                });
            plus(after, pill_y, pill_height)
        });

        let status = contents.status;
        let bottom = match &status {
            Status::Tabs { mode, note, panes } => lay_out_bottom(
                &geometry,
                (*mode, note.clone(), *panes),
                contents.tabs,
                contents.bottom_shown,
            ),
            Status::Typing { .. } => Bottom::default(),
        };
        let (palette, palette_text_x, choices) = match &status {
            Status::Typing { label, choices, .. } => {
                let (rect, rows) = place_palette(&geometry, choices.len());
                let label = label
                    .as_deref()
                    .map_or(0.0, |label| measure(label, false) + 2.0 * padding);
                (Some(rect), (rect.x + 2.0 * padding + label).round(), rows)
            }
            Status::Tabs { .. } => (None, 0.0, Vec::new()),
        };
        let menu = contents
            .menu
            .map(|(at, labels)| place_menu(&geometry, at, labels, measure));
        Self {
            geometry,
            workspaces,
            buttons,
            tabs: bottom.tabs,
            status,
            mode: bottom.mode,
            notes: bottom.notes,
            palette,
            palette_text_x,
            choices,
            new_workspace,
            new_tab: bottom.new_tab,
            menu,
            alert,
            bottom_shown: contents.bottom_shown,
            rounded: contents.rounded,
            finder: (contents.finder).map(|view| place_finder(&geometry, view, measure)),
        }
    }

    /// What `point` is over. `maximised` windows can't be resized from
    /// their edges.
    pub(crate) fn hit(&self, point: [f32; 2], maximised: bool) -> Hit {
        if let Some((rect, items)) = &self.menu
            && rect.contains(point)
        {
            return items
                .iter()
                .position(|(_, item)| item.contains(point))
                .map_or(Hit::Nothing, Hit::Menu);
        }
        if let Some((layout, view)) = &self.finder
            && layout.panel.contains(point)
        {
            return Hit::Finder(layout.row_at(point, view.rows.len()));
        }
        if !maximised && let Some(direction) = self.edge(point) {
            return Hit::Resize(direction);
        }
        if self.new_workspace.is_some_and(|rect| rect.contains(point)) {
            return Hit::NewWorkspace;
        }
        if self.new_tab.is_some_and(|rect| rect.contains(point)) {
            return Hit::NewTab;
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
            Hit::Panes
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
    /// of the bars, under the tiles, and those of the palette, over them.
    pub(crate) fn paint(&self, hover: Hit) -> Painted<'_> {
        let metrics = self.geometry.metrics;
        let mut painted = Painted::default();
        for (index, item) in (0_u8..).zip(&self.workspaces) {
            paint_workspace(
                item,
                index,
                hover == Hit::Workspace(item.id),
                &self.geometry,
                &mut painted,
            );
        }
        for &(button, rect) in &self.buttons {
            self.paint_button(button, rect, hover == Hit::Button(button), &mut painted);
        }
        if let Some((text, rect)) = &self.alert {
            paint_alert(text, *rect, &self.geometry, &mut painted);
        }
        if let Some(rect) = self.new_workspace {
            paint_plus(rect, hover == Hit::NewWorkspace, metrics, &mut painted);
        }
        if let Some(rect) = self.new_tab {
            paint_plus(rect, hover == Hit::NewTab, metrics, &mut painted);
        }
        if self.bottom_shown {
            let bottom = self.geometry.bottom;
            let text_y = (bottom.y + (bottom.height - metrics.cell[1]) / 2.0).round();
            if let Some((mode, rect)) = self.mode {
                self.push_bottom_fill(rect, Paint::Mode(mode), &mut painted.fills);
                painted.texts.push(Text {
                    origin: [rect.x + 3.0 * metrics.padding, text_y],
                    content: mode.text(),
                    ink: Ink::OnColor,
                    font: TextFont::Mono,
                });
            }
            for item in &self.tabs {
                let hovered = hover == Hit::Tab(item.id);
                let background = match (item.active, hovered) {
                    (true, _) => Some(Paint::Picked),
                    (false, true) => Some(Paint::Hover),
                    (false, false) => None,
                };
                if let Some(paint) = background {
                    self.push_bottom_fill(item.rect, paint, &mut painted.fills);
                }
                painted.texts.push(Text {
                    origin: [item.rect.x + 3.0 * metrics.padding, text_y],
                    content: &item.text,
                    ink: if item.active || hovered {
                        Ink::Plain
                    } else {
                        Ink::Muted
                    },
                    font: TextFont::Mono,
                });
            }
            for (note, x) in &self.notes {
                painted.texts.push(Text {
                    origin: [*x, text_y],
                    content: note,
                    ink: Ink::Muted,
                    font: TextFont::Mono,
                });
            }
        }
        self.paint_palette(&mut painted);
        if let Some((layout, view)) = &self.finder {
            layout.paint(view, &self.geometry, &mut painted);
        }
        if let Some((rect, items)) = &self.menu {
            push_shadow(
                &mut painted.over,
                *rect,
                self.geometry.radius,
                metrics.padding,
            );
            painted.over.push(
                Fill::rounded(rect.to_array(), Paint::Palette, self.geometry.radius)
                    .bordered(self.geometry.edge, Paint::PaletteEdge),
            );
            for (index, (label, item)) in items.iter().enumerate() {
                if hover == Hit::Menu(index) {
                    painted.over.push(Fill::rounded(
                        item.to_array(),
                        Paint::Picked,
                        (1.5 * metrics.padding).round(),
                    ));
                }
                painted.over_texts.push(Text {
                    origin: [
                        item.x + 3.0 * metrics.padding,
                        (item.y + (item.height - metrics.ui_line) / 2.0).round(),
                    ],
                    content: label,
                    ink: Ink::Plain,
                    font: TextFont::Ui,
                });
            }
        }
        painted
    }

    /// Fills `rect`, in the bottom bar, with `paint`: square, but for a
    /// corner that's the window's own rounded corner, which it follows.
    fn push_bottom_fill(&self, rect: Rect, paint: Paint, fills: &mut Vec<Fill>) {
        if !(self.rounded && rect.x <= 0.0) {
            fills.push(Fill::solid(rect.to_array(), paint));
            return;
        }
        let radius = self
            .geometry
            .window_radius
            .min(rect.height / 2.0)
            .min(rect.width / 2.0);
        let Rect {
            x,
            y,
            width,
            height,
        } = rect;
        // Square above the corner and right of it, and round in it.
        fills.push(Fill::solid([x, y, width, height - radius], paint));
        fills.push(Fill::solid(
            [x + radius, y + height - radius, width - radius, radius],
            paint,
        ));
        fills.push(Fill::rounded(
            [x, y + height - 2.0 * radius, 2.0 * radius, 2.0 * radius],
            paint,
            radius,
        ));
    }

    /// Where the text typed in the palette starts, its line's top left,
    /// while something's being typed.
    pub(crate) fn palette_text(&self) -> Option<[f32; 2]> {
        let rect = self.palette?;
        Some([
            self.palette_text_x,
            palette_text_y(rect, self.geometry.metrics),
        ])
    }

    /// The palette, over the tiles, while something's being typed.
    fn paint_palette<'a>(&'a self, painted: &mut Painted<'a>) {
        let metrics = self.geometry.metrics;
        if let (
            Some(rect),
            Status::Typing {
                label,
                text,
                choices,
            },
        ) = (self.palette, &self.status)
        {
            push_shadow(
                &mut painted.over,
                rect,
                self.geometry.radius,
                metrics.padding,
            );
            painted.over.push(
                Fill::rounded(rect.to_array(), Paint::Palette, self.geometry.radius)
                    .bordered(self.geometry.edge, Paint::PaletteEdge),
            );
            let line_height = (metrics.cell[1] + 4.0 * metrics.padding).round();
            if let Some(label) = label {
                painted.over_texts.push(Text {
                    origin: [
                        rect.x + 2.0 * metrics.padding,
                        (rect.y + (line_height - metrics.ui_line) / 2.0).round(),
                    ],
                    content: label,
                    ink: Ink::Muted,
                    font: TextFont::Ui,
                });
            }
            painted.over_texts.push(Text {
                origin: [self.palette_text_x, palette_text_y(rect, metrics)],
                content: text,
                ink: Ink::Plain,
                font: TextFont::Mono,
            });
            for ((choice, picked), row) in choices.iter().zip(&self.choices) {
                if *picked {
                    painted.over.push(Fill::rounded(
                        row.to_array(),
                        Paint::Picked,
                        (1.5 * metrics.padding).round(),
                    ));
                }
                painted.over_texts.push(Text {
                    origin: [
                        row.x + 2.0 * metrics.padding,
                        (row.y + (row.height - metrics.ui_line) / 2.0).round(),
                    ],
                    content: choice,
                    ink: if *picked { Ink::Plain } else { Ink::Muted },
                    font: TextFont::Ui,
                });
            }
        }
    }

    /// A button: its icon alone, a line for minimise, a square for
    /// maximise and a cross for close, on a soft circle while the mouse is
    /// over it.
    fn paint_button(&self, button: Button, rect: Rect, hovered: bool, painted: &mut Painted<'_>) {
        let metrics = self.geometry.metrics;
        let stroke = self.geometry.edge;
        if hovered {
            painted.fills.push(Fill::rounded(
                rect.to_array(),
                Paint::Picked,
                rect.width / 2.0,
            ));
        }
        let side = (rect.width * 0.36).round();
        let left = (rect.x + (rect.width - side) / 2.0).round();
        let top = (rect.y + (rect.height - side) / 2.0).round();
        match button {
            Button::Minimise => painted.fills.push(Fill::solid(
                [left, top + side - stroke, side, stroke],
                Paint::Icon,
            )),
            Button::Maximise => painted.fills.push(
                Fill::rounded([left, top, side, side], Paint::Clear, stroke)
                    .bordered(stroke, Paint::Icon),
            ),
            Button::Close => painted.texts.push(Text {
                origin: [
                    (rect.x + rect.width / 2.0 - metrics.ui_line * 0.27).round(),
                    (rect.y + (rect.height - metrics.ui_line) / 2.0).round(),
                ],
                content: "×",
                ink: if hovered { Ink::Plain } else { Ink::Muted },
                font: TextFont::Ui,
            }),
        }
    }
}

/// The top of the line of text typed in the palette at `rect`.
fn palette_text_y(rect: Rect, metrics: Metrics) -> f32 {
    let line_height = (metrics.cell[1] + 4.0 * metrics.padding).round();
    rect.y + (line_height - metrics.cell[1]) / 2.0
}

/// What the bottom bar holds, and where.
#[derive(Default)]
struct Bottom {
    mode: Option<(ModeName, Rect)>,
    tabs: Vec<Item<TabId>>,
    new_tab: Option<Rect>,
    notes: Vec<(String, f32)>,
}

/// Lays out the bottom bar, in the terminal's font, a cell per character:
/// the `mode` at its left, then the `tabs` and, if `shown`, a `+`, and at
/// its right the `note`, how many `panes` the tab has, and a hint. Where
/// the tabs need the room, the hint goes first, then the panes.
fn lay_out_bottom(
    geometry: &Geometry,
    (mode, note, panes): (Option<ModeName>, Option<String>, usize),
    tabs: Vec<(TabId, String, bool)>,
    shown: bool,
) -> Bottom {
    let metrics = geometry.metrics;
    let padding = metrics.padding;
    let bottom = geometry.bottom;
    let mono = |text: &str| {
        #[allow(clippy::cast_precision_loss)]
        let cells = crate::render::mono_width(text) as f32;
        cells * metrics.cell[0]
    };
    let mode = mode.map(|mode| {
        let rect = Rect {
            x: 0.0,
            y: bottom.y,
            width: (mono(mode.text()) + 6.0 * padding).round(),
            height: bottom.height,
        };
        (mode, rect)
    });
    let panes = match panes {
        0 => None,
        1 => Some("1 pane".to_owned()),
        panes => Some(format!("{panes} panes")),
    };
    let hint = mode.map(|(mode, _)| mode.hint().to_owned());
    let tabs_start = mode.map_or(0.0, |(_, rect)| rect.x + rect.width);
    let tab_width = |text: &str| (mono(text) + 6.0 * padding).round();
    let needed = tabs.iter().map(|(_, text, _)| tab_width(text)).sum::<f32>() + bottom.height;
    let mut wanted = [note, panes, hint];
    let (notes, notes_start) = loop {
        let mut x = bottom.width - 3.0 * padding;
        let mut notes = Vec::new();
        for text in wanted.iter().rev().flatten() {
            x = (x - mono(text)).round();
            notes.push((text.clone(), x));
            x -= 4.0 * padding;
        }
        let crowded = tabs_start + needed > x;
        match wanted.iter().rposition(Option::is_some) {
            Some(last) if crowded && last > 0 => wanted[last] = None,
            _ => {
                notes.reverse();
                break (notes, x);
            }
        }
    };
    let tabs = lay_out(
        tabs.into_iter()
            .map(|(id, text, active)| (id, text, None, active)),
        tabs_start..notes_start - bottom.height,
        0.0,
        |text, _, _| tab_width(text),
        (bottom.y, bottom.height),
    );
    let new_tab = shown.then(|| {
        let after = tabs
            .last()
            .map_or(tabs_start, |item| item.rect.x + item.rect.width);
        Rect {
            x: after,
            y: bottom.y,
            width: bottom.height,
            height: bottom.height,
        }
    });
    Bottom {
        mode,
        tabs,
        new_tab,
        notes,
    }
}

/// Places the window's buttons at the ends of the top bar, where the
/// desktop's `layout` says. Returns them, and where the space between them
/// starts and ends.
fn place_buttons(geometry: &Geometry, layout: &ButtonLayout) -> (Vec<(Button, Rect)>, f32, f32) {
    let metrics = geometry.metrics;
    let padding = metrics.padding;
    let top = geometry.top;
    let diameter = (metrics.pill_height() * 0.8).round();
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
    for &kind in &layout.left {
        buttons.push((kind, button(x)));
        x += diameter + spacing;
    }
    let start = if layout.left.is_empty() {
        2.0 * padding
    } else {
        x + padding
    };
    let mut x = top.width - 3.0 * padding;
    for &kind in layout.right.iter().rev() {
        x -= diameter;
        buttons.push((kind, button(x)));
        x -= spacing;
    }
    (buttons, start, x - padding)
}

/// The palette: in the middle of the window, wide enough for most
/// commands, a line of text tall, and `choices` rows taller. Returns it,
/// and where each choice goes.
fn place_palette(geometry: &Geometry, choices: usize) -> (Rect, Vec<Rect>) {
    let metrics = geometry.metrics;
    let padding = metrics.padding;
    let [width, height] = geometry.size;
    let palette_width = (width * 0.6)
        .min(90.0 * metrics.cell[0])
        .min(width - 4.0 * padding);
    let line_height = (metrics.cell[1] + 4.0 * padding).round();
    let row_height = metrics.pill_height();
    // As many choices as fit in the window.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let fit = ((height - line_height - 5.0 * padding) / row_height).max(0.0) as usize;
    let choices = choices.min(fit);
    #[allow(clippy::cast_precision_loss)]
    let list_height = if choices == 0 {
        0.0
    } else {
        choices as f32 * row_height + padding
    };
    // Its text in the middle, unless the choices would run off the bottom.
    let total = line_height + list_height;
    let y = ((height - line_height) / 2.0)
        .min(height - total - 2.0 * padding)
        .max(2.0 * padding)
        .round();
    let palette = Rect {
        x: ((width - palette_width) / 2.0).round(),
        y,
        width: palette_width.round(),
        height: total,
    };
    let rows = (0_u16..)
        .take(choices)
        .map(|index| Rect {
            x: palette.x + padding,
            y: palette.y + line_height + f32::from(index) * row_height,
            width: palette.width - 2.0 * padding,
            height: row_height,
        })
        .collect();
    (palette, rows)
}

/// The finder, showing `view`, its title measured by `measure`.
fn place_finder(
    geometry: &Geometry,
    view: FinderView,
    measure: &mut dyn FnMut(&str, bool) -> f32,
) -> (finder::view::Layout, FinderView) {
    let mut layout = finder::view::Layout::new(geometry);
    layout.title_width = measure(view.title, false);
    (layout, view)
}

/// A menu of `labels` asked for at `at`: below and right of it, unless
/// that runs off the window. Returns its box, and its items.
fn place_menu(
    geometry: &Geometry,
    at: [f32; 2],
    labels: Vec<String>,
    measure: &mut dyn FnMut(&str, bool) -> f32,
) -> (Rect, Vec<(String, Rect)>) {
    let padding = geometry.metrics.padding;
    let item_height = geometry.metrics.pill_height();
    let widest = labels
        .iter()
        .map(|label| measure(label, false))
        .fold(0.0, f32::max);
    let width = (widest + 8.0 * padding).round();
    #[allow(clippy::cast_precision_loss)]
    let height = labels.len() as f32 * item_height + 2.0 * padding;
    let [window_width, window_height] = geometry.size;
    let x = at[0]
        .min(window_width - width - padding)
        .max(padding)
        .round();
    let y = at[1]
        .min(window_height - height - padding)
        .max(padding)
        .round();
    let items = (0_u8..)
        .zip(labels)
        .map(|(index, label)| {
            let rect = Rect {
                x: x + padding,
                y: y + padding + f32::from(index) * item_height,
                width: width - 2.0 * padding,
                height: item_height,
            };
            (label, rect)
        })
        .collect();
    let menu = Rect {
        x,
        y,
        width,
        height,
    };
    (menu, items)
}

/// Adds a soft shadow under `rect`, whose corners are `radius` round:
/// layers wider and fainter outwards, a little below it.
pub(crate) fn push_shadow(fills: &mut Vec<Fill>, rect: Rect, radius: f32, padding: f32) {
    for spread in [3.0, 2.0, 1.0] {
        let grow = spread * padding;
        fills.push(Fill::rounded(
            [
                rect.x - grow,
                rect.y - grow + padding,
                rect.width + 2.0 * grow,
                rect.height + 2.0 * grow,
            ],
            Paint::Shadow,
            radius + grow,
        ));
    }
}

/// A `+`, which makes a workspace or opens a tab, on a soft background
/// while the mouse is over it.
fn paint_plus(rect: Rect, hovered: bool, metrics: Metrics, painted: &mut Painted<'_>) {
    if hovered {
        painted.fills.push(Fill::rounded(
            rect.to_array(),
            Paint::Hover,
            (1.5 * metrics.padding).round(),
        ));
    }
    let side = (metrics.ui_line * 0.6).round();
    let stroke = (metrics.padding / 4.0).round().max(1.0);
    let left = (rect.x + (rect.width - side) / 2.0).round();
    let top = (rect.y + (rect.height - side) / 2.0).round();
    let middle = |at: f32| (at + (side - stroke) / 2.0).round();
    let paint = if hovered { Paint::Ink } else { Paint::Icon };
    painted
        .fills
        .push(Fill::solid([left, middle(top), side, stroke], paint));
    painted
        .fills
        .push(Fill::solid([middle(left), top, stroke, side], paint));
}

/// What the frame draws: under the tiles, and over them.
#[derive(Debug, Default)]
pub(crate) struct Painted<'a> {
    pub(crate) fills: Vec<Fill>,
    pub(crate) texts: Vec<Text<'a>>,
    /// Over the tiles and their text, such as the palette.
    pub(crate) over: Vec<Fill>,
    pub(crate) over_texts: Vec<Text<'a>>,
}

/// Draws an alert: in the warning colour, on a faint wash of it.
fn paint_alert<'a>(text: &'a str, rect: Rect, geometry: &Geometry, painted: &mut Painted<'a>) {
    let metrics = geometry.metrics;
    painted.fills.push(
        Fill::rounded(
            rect.to_array(),
            Paint::Warning,
            (1.5 * metrics.padding).round(),
        )
        .bordered(geometry.edge, Paint::WarningEdge),
    );
    painted.texts.push(Text {
        origin: [
            rect.x + 3.0 * metrics.padding,
            (rect.y + (rect.height - metrics.ui_line) / 2.0).round(),
        ],
        content: text,
        ink: Ink::Warning,
        font: TextFont::Ui,
    });
}

/// Draws a workspace: its dot in its colour, its name, and how many tabs
/// it has, on an edged pill if it's the one shown, a soft one if the mouse
/// is over it, and in quieter text otherwise.
fn paint_workspace<'a>(
    item: &'a Item<WorkspaceId>,
    index: u8,
    hovered: bool,
    geometry: &Geometry,
    painted: &mut Painted<'a>,
) {
    let metrics = geometry.metrics;
    let padding = metrics.padding;
    let radius = (1.5 * padding).round();
    let rect = item.rect;
    if item.active {
        painted.fills.push(
            Fill::rounded(rect.to_array(), Paint::Picked, radius)
                .bordered(geometry.edge, Paint::PickedEdge),
        );
    } else if hovered {
        painted
            .fills
            .push(Fill::rounded(rect.to_array(), Paint::Hover, radius));
    }
    let dot = metrics.dot();
    let dot_x = (rect.x + 3.0 * padding).round();
    painted.fills.push(Fill::rounded(
        [
            dot_x,
            (rect.y + (rect.height - dot) / 2.0).round(),
            dot,
            dot,
        ],
        Paint::Workspace(index),
        dot / 2.0,
    ));
    let text_y = (rect.y + (rect.height - metrics.ui_line) / 2.0).round();
    let text_x = dot_x + dot + 2.0 * padding;
    painted.texts.push(Text {
        origin: [text_x, text_y],
        content: &item.text,
        ink: if item.active || hovered {
            Ink::Plain
        } else {
            Ink::Muted
        },
        font: TextFont::Ui,
    });
    if let Some((detail, x)) = &item.detail {
        painted.texts.push(Text {
            origin: [*x, text_y],
            content: detail,
            ink: Ink::Muted,
            font: TextFont::Ui,
        });
    }
}

/// The window behind everything, in the bars' colour: rounded where it can
/// be see-through and isn't maximised, with a faint line round it then.
pub(crate) fn window_fill(geometry: &Geometry, rounded: bool) -> Fill {
    let [width, height] = geometry.size;
    let fill = Fill::rounded(
        [0.0, 0.0, width, height],
        Paint::Bar,
        if rounded { geometry.window_radius } else { 0.0 },
    );
    if rounded {
        fill.bordered(geometry.edge, Paint::WindowEdge)
    } else {
        fill
    }
}

/// Each of the first nine panes' numbers, big in the middle of it, while
/// Ctrl+B p waits for one: the fills, and the numbers.
pub(crate) fn paint_pane_numbers(
    geometry: &Geometry,
    placement: &Placement,
) -> (Vec<Fill>, Vec<Text<'static>>) {
    const NUMBERS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
    let metrics = geometry.metrics;
    let [cell_width, cell_height] = metrics.cell;
    let side = (2.0 * cell_height).round();
    let mut fills = Vec::new();
    let mut texts = Vec::new();
    for ((_, tile), number) in placement.panes.iter().zip(NUMBERS) {
        let rect = tile.rect;
        let x = (rect.x + (rect.width - side) / 2.0).round();
        let y = (rect.y + (rect.height - side) / 2.0).round();
        fills.push(Fill::rounded(
            [x, y, side, side],
            Paint::Accent,
            (1.5 * metrics.padding).round(),
        ));
        texts.push(Text {
            origin: [
                (x + (side - cell_width) / 2.0).round(),
                (y + (side - cell_height) / 2.0).round(),
            ],
            content: number,
            ink: Ink::OnColor,
            font: TextFont::Mono,
        });
    }
    (fills, texts)
}

/// An editor's panel by its cursor, as `view` places it in the pane whose
/// cells start at `origin`: raised over the panes as the palette is, with
/// its text in the terminal's font, code on a faint band or box, and a
/// scroll bar if there's more than it shows. Its fills go in `over`, and
/// its text in `texts`.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn paint_panel<'a>(
    geometry: &Geometry,
    origin: [f32; 2],
    view: &PanelView<'a>,
    over: &mut Vec<Fill>,
    texts: &mut Vec<Text<'a>>,
) {
    let metrics = geometry.metrics;
    let [cell_width, cell_height] = metrics.cell;
    let placed = view.placed;
    // A cell of room either side of the text, and half a cell above and
    // below.
    let rect = Rect {
        x: (origin[0] + placed.left as f32 * cell_width).round(),
        y: (origin[1] + placed.top as f32 * cell_height).round(),
        width: ((placed.width + 2) as f32 * cell_width).round(),
        height: ((placed.height + 1) as f32 * cell_height).round(),
    };
    push_shadow(over, rect, geometry.radius, metrics.padding);
    over.push(
        Fill::rounded(rect.to_array(), Paint::Palette, geometry.radius)
            .bordered(geometry.edge, Paint::PaletteEdge),
    );
    let left = rect.x + cell_width;
    let top = (rect.y + cell_height / 2.0).round();
    for (index, line) in view.lines.iter().enumerate() {
        let y = top + index as f32 * cell_height;
        if line.block {
            over.push(Fill::solid(
                [
                    rect.x + geometry.edge,
                    y,
                    rect.width - 2.0 * geometry.edge,
                    cell_height,
                ],
                Paint::Code,
            ));
        }
        if line.picked {
            over.push(Fill::rounded(
                [
                    rect.x + geometry.edge,
                    y,
                    rect.width - 2.0 * geometry.edge,
                    cell_height,
                ],
                Paint::Picked,
                metrics.padding,
            ));
        }
        let mut col = 0;
        for run in &line.runs {
            let x = left + col as f32 * cell_width;
            let cells = crate::render::mono_width(&run.text);
            if run.code {
                over.push(Fill::rounded(
                    [
                        x - 1.0,
                        y + 1.0,
                        cells as f32 * cell_width + 2.0,
                        cell_height - 2.0,
                    ],
                    Paint::Code,
                    metrics.padding,
                ));
            }
            texts.push(Text {
                origin: [x, y],
                content: &run.text,
                ink: run.ink,
                font: TextFont::Mono,
            });
            col += cells;
        }
    }
    // Where what's shown is in all of it, at the right, in the room
    // beside the text.
    if view.total > view.lines.len() {
        let track = view.lines.len() as f32 * cell_height;
        let thumb = (track * view.lines.len() as f32 / view.total as f32).max(cell_height / 2.0);
        let y = top + (track - thumb) * view.first as f32 / (view.total - view.lines.len()) as f32;
        let width = (metrics.padding * 0.75).max(2.0);
        over.push(Fill::rounded(
            [
                rect.x + rect.width - f32::midpoint(cell_width, width),
                y,
                width,
                thumb,
            ],
            Paint::PaletteEdge,
            width / 2.0,
        ));
    }
}

/// The panes of `placement`: each one's background, side by side, with
/// lines between them and the bars, and round `focused`, if there are
/// others to tell it from, a line in clackatoo's colour. Round
/// `pointed`, a pane an agent points out, a line in the picked colour.
pub(crate) fn paint_panes(
    geometry: &Geometry,
    placement: &Placement,
    focused: Option<PaneId>,
    pointed: Option<PaneId>,
) -> Vec<Fill> {
    let line = geometry.edge;
    let area = geometry.area.rect;
    // The lines are what shows between the panes' backgrounds.
    let mut fills = vec![Fill::solid(
        [area.x, area.y - line, area.width, area.height + 2.0 * line],
        Paint::Divider,
    )];
    for (_, tile) in &placement.panes {
        fills.push(Fill::solid(tile.rect.to_array(), Paint::Tile));
    }
    if placement.panes.len() > 1
        && let Some(tile) = focused.and_then(|pane| placement.tile(pane))
    {
        // Over the lines round it.
        let rect = tile.rect;
        fills.push(
            Fill::solid(
                [
                    rect.x - line,
                    rect.y - line,
                    rect.width + 2.0 * line,
                    rect.height + 2.0 * line,
                ],
                Paint::Clear,
            )
            .bordered(line, Paint::FocusEdge),
        );
    }
    if let Some(tile) = pointed.and_then(|pane| placement.tile(pane)) {
        let rect = tile.rect;
        let width = 2.0 * line;
        fills.push(
            Fill::solid(
                [
                    rect.x - width,
                    rect.y - width,
                    rect.width + 2.0 * width,
                    rect.height + 2.0 * width,
                ],
                Paint::Clear,
            )
            .bordered(width, Paint::PickedEdge),
        );
    }
    fills
}

/// Places `items` side by side across `span`, `spacing` apart, at `y`
/// and `height` high, each as wide as `width` says for its text and
/// detail. Those that would pass the span's end are left out.
fn lay_out<T>(
    items: impl IntoIterator<Item = (T, String, Option<String>, bool)>,
    span: std::ops::Range<f32>,
    spacing: f32,
    mut width: impl FnMut(&str, Option<&str>, bool) -> f32,
    (y, height): (f32, f32),
) -> Vec<Item<T>> {
    let mut x = span.start;
    let mut placed = Vec::new();
    for (id, text, detail, active) in items {
        let item_width = width(&text, detail.as_deref(), active);
        if x + item_width > span.end {
            break;
        }
        placed.push(Item {
            id,
            text,
            detail: detail.map(|detail| (detail, 0.0)),
            active,
            rect: Rect {
                x,
                y,
                width: item_width,
                height,
            },
        });
        x += item_width + spacing;
    }
    placed
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
    /// The space the tiles share.
    Panes,
    /// The `+` that makes a workspace.
    NewWorkspace,
    /// The `+` that opens a tab.
    NewTab,
    /// An item of the open menu, by its place in it.
    Menu(usize),
    /// The finder, and the row of its list, if it's over one. The panes
    /// under it get nothing.
    Finder(Option<usize>),
    Nothing,
}

#[cfg(test)]
// Every position here is a whole number of pixels, so exact.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// Cells 10 × 20, interface text 16 high, and a padding of 4: names in
    /// the top bar are 24 high, the top bar 36 and the bottom bar 26.
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
                    (WorkspaceId(1), "clackatoo".into(), 2, true),
                    (WorkspaceId(2), "peios".into(), 1, false),
                ],
                tabs: vec![
                    (TabId(1), "1 zsh".into(), true),
                    (TabId(2), "2 htop".into(), false),
                ],
                status,
                buttons,
                bottom_shown: true,
                menu: None,
                alert: None,
                rounded: false,
                finder: None,
            },
            &mut measure,
        )
    }

    #[test]
    fn an_alert_goes_before_the_buttons_and_the_workspaces_make_room() {
        let buttons = ButtonLayout::default();
        let workspaces: Vec<_> = (1..=12)
            .map(|id| (WorkspaceId(id), format!("workspace {id}"), 1, id == 1))
            .collect();
        let chrome = Chrome::new(
            Geometry::new([800, 600], METRICS),
            Contents {
                workspaces,
                tabs: Vec::new(),
                status: Status::tabs(),
                buttons: &buttons,
                bottom_shown: true,
                menu: None,
                alert: Some("no contact with box for 4 s".into()),
                rounded: false,
                finder: None,
            },
            &mut measure,
        );
        let (_, alert) = chrome.alert.clone().expect("an alert");
        let first_button = chrome
            .buttons
            .iter()
            .map(|(_, rect)| rect.x)
            .fold(f32::INFINITY, f32::min);
        // 27 characters of 8, and three paddings either side.
        assert_eq!(alert.width, 27.0 * 8.0 + 24.0);
        assert!(alert.x + alert.width <= first_button - METRICS.padding);
        let last = chrome.workspaces.last().expect("some workspaces fit");
        assert!(last.rect.x + last.rect.width < alert.x);
        assert!(chrome.workspaces.len() < 12, "the rest are left out");
        // It's part of the bar, which drags the window.
        assert_eq!(
            chrome.hit([alert.x + 2.0, alert.y + 2.0], false),
            Hit::TopBar
        );
    }

    #[test]
    fn the_panes_fill_the_space_between_the_bars() {
        let geometry = Geometry::new([800, 600], METRICS);
        assert_eq!((geometry.top.height, geometry.bottom.y), (36.0, 574.0));
        // Edge to edge, between a line under the top bar and one over the
        // bottom bar.
        assert_eq!(
            geometry.area.rect,
            Rect {
                x: 0.0,
                y: 37.0,
                width: 800.0,
                height: 536.0
            }
        );
        // 100 × 30 cells, inset 12 in the pane, and the lines.
        assert_eq!(Geometry::window_size(100, 30, METRICS), [1024.0, 688.0]);
    }

    #[test]
    fn points_hit_what_they_are_over() {
        let chrome = chrome(
            Status::Tabs {
                mode: Some(ModeName::Normal),
                note: None,
                panes: 2,
            },
            &ButtonLayout::default(),
        );
        // clackatoo's pill starts two paddings in, 8, and holds a dot of 6,
        // 9 characters, its 1 character of tabs 6 after them, and 32 of
        // padding: 124 wide.
        assert_eq!(chrome.workspaces[0].rect.x, 8.0);
        assert_eq!(chrome.workspaces[0].rect.width, 124.0);
        let (count, count_x) = chrome.workspaces[0].detail.clone().unwrap();
        assert_eq!((count.as_str(), count_x), ("2", 8.0 + 124.0 - 12.0 - 8.0));
        assert_eq!(
            chrome.hit([20.0, 18.0], false),
            Hit::Workspace(WorkspaceId(1))
        );
        assert_eq!(
            chrome.hit([150.0, 18.0], false),
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
        // The mode comes first in the bottom bar, its 6 cells and 24 of
        // padding, then the tabs.
        let (_, mode) = chrome.mode.unwrap();
        assert_eq!((mode.x, mode.width), (0.0, 84.0));
        assert_eq!(chrome.hit([20.0, 584.0], false), Hit::Nothing);
        assert_eq!(chrome.hit([100.0, 584.0], false), Hit::Tab(TabId(1)));
        // The notes end the bar: how many panes, then a hint.
        let notes: Vec<&str> = chrome.notes.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(notes, ["2 panes", ": command"]);
        assert_eq!(chrome.hit([400.0, 584.0], false), Hit::Nothing);
        assert_eq!(chrome.hit([100.0, 100.0], false), Hit::Panes);
    }

    #[test]
    fn workspaces_named_with_a_number_dont_count_their_tabs() {
        let buttons = ButtonLayout::default();
        let chrome = Chrome::new(
            Geometry::new([800, 600], METRICS),
            Contents {
                workspaces: vec![
                    (WorkspaceId(1), "1".into(), 3, true),
                    (WorkspaceId(2), "work 2".into(), 3, false),
                ],
                tabs: Vec::new(),
                status: Status::tabs(),
                buttons: &buttons,
                bottom_shown: true,
                menu: None,
                alert: None,
                rounded: false,
                finder: None,
            },
            &mut measure,
        );
        assert_eq!(chrome.workspaces[0].detail, None);
        assert_eq!(
            chrome.workspaces[1]
                .detail
                .as_ref()
                .map(|(count, _)| count.as_str()),
            Some("3")
        );
    }

    #[test]
    fn buttons_on_the_left_push_the_workspaces_over() {
        let buttons = ButtonLayout {
            left: vec![Button::Close],
            right: Vec::new(),
        };
        let chrome = chrome(Status::tabs(), &buttons);
        let close = chrome.buttons[0].1;
        assert_eq!(close.x, 12.0);
        assert!(chrome.workspaces[0].rect.x > close.x + close.width);
    }

    #[test]
    fn edges_and_corners_resize_unless_maximised() {
        let chrome = chrome(Status::tabs(), &ButtonLayout::default());
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
            Status::Typing {
                label: None,
                text: ":|Terminal".into(),
                choices: Vec::new(),
            },
            &ButtonLayout::default(),
        );
        assert!(chrome.tabs.is_empty());
        let palette = chrome.palette.expect("a palette");
        assert_eq!(palette.x + palette.width / 2.0, 400.0);
        assert_eq!(palette.y + palette.height / 2.0, 300.0);
    }
}
