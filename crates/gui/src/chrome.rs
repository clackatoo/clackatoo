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
//!
//! The tabs and the workspaces each run in a [strip](strip) that scrolls
//! when there are more than fit. A message in the bottom bar takes only the
//! room the tabs leave, cut short if it must be, with the whole of it in a
//! box raised above the bar when the mouse is over it.

mod keys;
mod strip;

pub(crate) use strip::{End, Scroll};

use clackatoo_protocol::{TabId, WorkspaceId};
use winit::window::ResizeDirection;

use clackatoo_protocol::PaneId;

use crate::desktop::{Button, ButtonLayout};
use crate::editor::PanelView;
use crate::finder::{self, view::FinderView};
use crate::layout::{Area, Placement, Rect, Tile};
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
    /// Visual mode the mouse started from insert mode, which goes back
    /// there once it ends, as Neovim's "(insert) VISUAL".
    InsertVisual,
    InsertVisualLine,
}

impl ModeName {
    /// The modes the bar names most of the time, whose pill keeps one
    /// width: not the brief `(insert)` ones, which widen it while shown.
    const ORDINARY: [Self; 4] = [Self::Insert, Self::Normal, Self::Visual, Self::VisualLine];

    fn text(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Normal => "NORMAL",
            Self::Visual => "VISUAL",
            Self::VisualLine => "V-LINE",
            Self::InsertVisual => "(insert) VISUAL",
            Self::InsertVisualLine => "(insert) V-LINE",
        }
    }

    /// Where its name starts in its pill, `rect`: in the middle of the
    /// room kept for the widest ordinary name.
    fn text_x(self, rect: Rect, metrics: Metrics) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let width = crate::render::mono_width(self.text()) as f32 * metrics.cell[0];
        (rect.x + (rect.width - width) / 2.0).round()
    }

    /// Which of the theme's mode colours it's drawn on.
    pub(crate) fn colour(self) -> usize {
        match self {
            Self::Insert => 0,
            Self::Normal => 1,
            Self::Visual | Self::VisualLine | Self::InsertVisual | Self::InsertVisualLine => 2,
        }
    }

    /// A reminder of the key that does most from here.
    fn hint(self) -> &'static str {
        match self {
            Self::Insert => "^; normal mode",
            Self::Normal
            | Self::Visual
            | Self::VisualLine
            | Self::InsertVisual
            | Self::InsertVisualLine => ": command",
        }
    }
}

/// What a tab or a workspace says of its chats, after its name. When
/// several chats would say different things, the one that needs the user
/// most is said: asking, then at work, then finished unseen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Mark {
    /// A chat finished while this window wasn't showing it.
    Unseen,
    /// A chat's agent is at work.
    Working,
    /// A chat's agent asks the user something, and waits.
    Asking,
}

impl Mark {
    /// Every mark.
    pub(crate) const ALL: [Self; 3] = [Self::Unseen, Self::Working, Self::Asking];
    /// What goes after a name: a space and the mark.
    pub(crate) fn suffix(self) -> &'static str {
        match self {
            Self::Unseen => " •",
            Self::Working => " ⋯",
            Self::Asking => " ?",
        }
    }
}

/// A workspace, as the top bar names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceName {
    pub(crate) id: WorkspaceId,
    pub(crate) name: String,
    /// How many tabs it has.
    pub(crate) tabs: usize,
    /// Whether it's the one shown.
    pub(crate) shown: bool,
    /// What it says of its chats, after its name.
    pub(crate) mark: Option<Mark>,
}

impl WorkspaceName {
    /// How many tabs it has, as the top bar says it after its name, unless
    /// it's named with a number.
    fn tabs_said(&self) -> Option<String> {
        let name = &self.name;
        let numbered = !name.is_empty() && name.chars().all(|ch| ch.is_ascii_digit());
        (!numbered).then(|| self.tabs.to_string())
    }

    #[cfg(test)]
    /// Workspace `id`, called `name`, with `tabs` tabs, shown if `shown`,
    /// with nothing to say of its chats.
    pub(crate) fn plain(id: u32, name: &str, tabs: usize, shown: bool) -> Self {
        Self {
            id: WorkspaceId(id),
            name: name.to_owned(),
            tabs,
            shown,
            mark: None,
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
    /// What a workspace says of its chats, after its name, and where it
    /// starts.
    pub(crate) mark: Option<(Mark, f32)>,
}

/// What the bottom bar and the palette show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Something being typed, such as a command or a search, in the
    /// palette, perhaps after a label saying what it's for, and with
    /// choices listed under it, one of them picked; and what the bottom
    /// bar shows meanwhile, if anything: the tabs, and the mode.
    Typing {
        label: Option<String>,
        text: String,
        choices: Vec<(String, bool)>,
        /// The bytes of `text` selected, which what's typed first replaces.
        selected: Option<std::ops::Range<usize>>,
        under: Option<Box<Status>>,
    },
    /// The focused pane's mode, the tabs, and on the right a note (how far
    /// the view is scrolled, or a message), what kind of note it is, how
    /// many panes the tab has, and a hint.
    Tabs {
        mode: Option<ModeName>,
        note: Option<String>,
        kind: NoteKind,
        panes: usize,
    },
}

impl Status {
    /// What the bottom bar shows: what's under what's typed, if anything,
    /// or else this.
    pub(crate) fn bar(&self) -> &Self {
        match self {
            Self::Typing {
                under: Some(under), ..
            } => under,
            status => status,
        }
    }

    /// The tabs, with nothing else to say.
    #[cfg(test)]
    pub(crate) fn tabs() -> Self {
        Self::Tabs {
            mode: None,
            note: None,
            kind: NoteKind::Label,
            panes: 1,
        }
    }
}

/// What the note at the bottom bar's right is, which says how much room it
/// gets and how it's drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum NoteKind {
    /// What the focused pane is, such as a file's name and the cursor's
    /// line, or how far the view is scrolled: quiet, in the room the tabs
    /// leave.
    #[default]
    Label,
    /// Something said until the next key, such as `not found`: in the room
    /// the tabs leave.
    Message,
    /// Something gone wrong, said until the next key: in the warning
    /// colour, and if it's cut short, whole in a box above the bar while
    /// `notice` holds, a few seconds.
    Warning { notice: bool },
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
    /// The keys waiting for the rest of a binding, written short after the
    /// mode, as `^B`, and where.
    pub(crate) waiting: Option<(String, Rect)>,
    /// The panel of what can come next, above the bar, while keys wait.
    pub(crate) keys: Option<keys::KeysPanel>,
    /// Text at the bottom bar's right.
    pub(crate) notes: Vec<Note>,
    /// The counts of workspaces and tabs hidden either way, where the
    /// strips overflow.
    pub(crate) hidden: Vec<HiddenCount>,
    /// Where the strips are scrolled to now, for the window to keep.
    pub(crate) scrolls: Scrolls,
    /// Where they'd be scrolled to by a click on each count.
    scroll_to: Vec<(Hit, Scrolls)>,
    /// The note, if it's cut short, whose whole is raised above the bar
    /// while the mouse is over it.
    pub(crate) cut: Option<Cut>,
    /// Whether to show the cut note whole without the mouse, as for
    /// something gone wrong, said moments ago.
    notice: bool,
    /// The messages said lately, raised above the bar, while `:messages`
    /// shows them.
    pub(crate) messages: Option<Raised>,
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
    /// Each workspace's name, how many tabs it has, whether it's shown,
    /// and what it says of its chats.
    pub(crate) workspaces: Vec<WorkspaceName>,
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
    /// Where the strips of workspaces and tabs were scrolled to.
    pub(crate) scrolls: Scrolls,
    /// The messages said lately, oldest first, while `:messages` shows
    /// them.
    pub(crate) messages: Option<&'a [String]>,
    /// The keys waiting for the rest of a binding, written short, as the
    /// bar marks them: `^B`.
    pub(crate) waiting: Option<String>,
    /// What can come next, while keys wait for the rest of a binding.
    pub(crate) which_keys: Option<crate::keymap::WhichKeys>,
}

/// Where the strips of workspaces and of tabs are scrolled to, which the
/// window keeps from one frame to the next.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Scrolls {
    pub(crate) workspaces: Scroll,
    pub(crate) tabs: Scroll,
}

/// Which bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bar {
    Top,
    Bottom,
}

/// Text at the bottom bar's right, where it starts, and how it's drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Note {
    pub(crate) text: String,
    pub(crate) x: f32,
    pub(crate) ink: Ink,
}

/// How many workspaces or tabs are hidden one way, as a strip's end says,
/// quietly: `‹3` before, `5›` after.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HiddenCount {
    pub(crate) bar: Bar,
    pub(crate) end: End,
    pub(crate) text: String,
    pub(crate) rect: Rect,
    /// Where its text starts, in the middle of it.
    pub(crate) text_x: f32,
}

/// The bottom bar's note, cut short: where it is, and the whole of it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cut {
    pub(crate) rect: Rect,
    whole: String,
    /// Whether it says something went wrong.
    warning: bool,
    /// The whole of it in a box above the bar, made the first time it's
    /// shown.
    raised: std::cell::OnceCell<Raised>,
}

impl Cut {
    /// The whole of it, in a box above the bar of the window `geometry`
    /// lays out.
    pub(crate) fn raised(&self, geometry: &Geometry) -> &Raised {
        self.raised.get_or_init(|| {
            raise(
                geometry,
                std::slice::from_ref(&self.whole),
                Some(self.rect.x),
                self.warning,
            )
        })
    }
}

/// Lines in a box raised above the bottom bar, at its right: a message
/// too long for the bar, whole, or the messages said lately.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Raised {
    pub(crate) rect: Rect,
    pub(crate) lines: Vec<String>,
    /// Whether it says something went wrong, in the warning colour.
    pub(crate) warning: bool,
}

impl Chrome {
    /// Lays out the frame: the buttons at the ends of the top bar where
    /// the desktop puts them, the workspaces from its left, and the mode
    /// and the tabs from the bottom bar's left, with notes at its right.
    /// `measure` says how wide interface text is, bold or not. Where there
    /// are more workspaces or tabs than fit, their strip is scrolled on
    /// from where `contents` says it was, to keep the one shown in view.
    #[allow(clippy::too_many_lines, reason = "each part of the frame in turn")]
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

        let (workspaces, workspace_strip) = lay_out_workspaces(
            &contents.workspaces,
            workspaces_start..workspaces_end - pill_height - padding,
            (metrics, pill_y, pill_height),
            measure,
            contents.scrolls.workspaces,
        );
        // The `+` after them, or with none, where they'd start.
        let gap = if workspace_strip.shown.is_empty() {
            0.0
        } else {
            padding
        };
        let new_workspace = contents.bottom_shown.then_some(Rect {
            x: workspace_strip.end + gap,
            y: pill_y,
            width: pill_height,
            height: pill_height,
        });

        let status = contents.status;
        let bottom = lay_out_bottom(
            &geometry,
            (&status, contents.waiting.as_deref()),
            &contents.tabs,
            contents.bottom_shown,
            contents.scrolls.tabs,
        );
        // Drawn only with the bottom bar, whose mark of the keys it explains.
        let keys = (contents.which_keys)
            .filter(|_| contents.bottom_shown)
            .map(|which| keys::KeysPanel::new(&geometry, which));
        let scrolls = Scrolls {
            workspaces: workspace_strip.scroll,
            tabs: bottom.scroll,
        };
        let (hidden, scroll_to) = count_hidden(
            [(Bar::Top, &workspace_strip), (Bar::Bottom, &bottom.strip)],
            scrolls,
            metrics,
            measure,
        );
        // Drawn only with the bottom bar, as notices take its place.
        let messages = (contents.messages).map(|lines| raise(&geometry, lines, None, false));
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
            notice: matches!(
                status.bar(),
                Status::Tabs {
                    kind: NoteKind::Warning { notice: true },
                    ..
                }
            ),
            status,
            mode: bottom.mode,
            waiting: bottom.waiting,
            keys,
            notes: bottom.notes,
            hidden,
            scrolls,
            scroll_to,
            cut: bottom.cut,
            messages,
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
        if let Some(count) = self.hidden.iter().find(|count| count.rect.contains(point))
            && (count.bar == Bar::Top || self.bottom_shown)
        {
            return Hit::Hidden(count.bar, count.end);
        }
        if self.bottom_shown
            && let Some(cut) = &self.cut
            && cut.rect.contains(point)
        {
            return Hit::Note;
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

    /// Where the strips would be scrolled to by a click on `hit`, if it's
    /// a count of workspaces or tabs hidden.
    pub(crate) fn scrolled(&self, hit: Hit) -> Option<Scrolls> {
        (self.scroll_to.iter())
            .find(|(at, _)| *at == hit)
            .map(|&(_, scrolls)| scrolls)
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

    /// Whether something is raised over the panes, with the mouse at
    /// `hover`: the palette, the finder, a menu, the keys a prefix takes,
    /// the messages said lately, or the note cut short, whole. What's
    /// drawn over panes, such as a terminal's scrolled pill, waits.
    pub(crate) fn raises_over_panes(&self, hover: Hit) -> bool {
        let messages = (self.messages.as_ref()).is_some_and(|raised| !raised.lines.is_empty());
        let cut = self.cut.is_some() && (hover == Hit::Note || self.notice);
        self.palette.is_some()
            || self.finder.is_some()
            || self.menu.is_some()
            || self.keys.is_some()
            || (self.bottom_shown && (messages || cut))
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
                    origin: [mode.text_x(rect, metrics), text_y],
                    content: mode.text(),
                    ink: Ink::OnColor,
                    font: TextFont::Mono,
                });
            }
            // On the bar itself, so it's not taken for a tab.
            if let Some((waiting, rect)) = &self.waiting {
                painted.texts.push(Text {
                    origin: [rect.x + 3.0 * metrics.padding, text_y],
                    content: waiting,
                    ink: Ink::Key,
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
            for note in &self.notes {
                painted.texts.push(Text {
                    origin: [note.x, text_y],
                    content: &note.text,
                    ink: note.ink,
                    font: TextFont::Mono,
                });
            }
        }
        for count in &self.hidden {
            self.paint_hidden(
                count,
                hover == Hit::Hidden(count.bar, count.end),
                &mut painted,
            );
        }
        // The messages said lately, while `:messages` shows them, or else
        // the note cut short, whole, while the mouse is over it or it's
        // just said something's gone wrong.
        let listed = self.messages.as_ref();
        let raised = listed
            .filter(|raised| !raised.lines.is_empty())
            .or_else(|| {
                let cut = self.cut.as_ref()?;
                (hover == Hit::Note || self.notice).then(|| cut.raised(&self.geometry))
            });
        if let Some(raised) = raised.filter(|_| self.bottom_shown) {
            paint_raised(raised, &self.geometry, &mut painted);
        }
        if let Some(keys) = &self.keys {
            keys.paint(&self.geometry, &mut painted);
        }
        self.paint_palette(&mut painted);
        if let Some((layout, view)) = &self.finder {
            layout.paint(view, &self.geometry, &mut painted);
        }
        self.paint_menu(hover, &mut painted);
        painted
    }

    /// The menu, if one is open, over everything, with the item under the
    /// mouse picked out.
    fn paint_menu<'a>(&'a self, hover: Hit, painted: &mut Painted<'a>) {
        let metrics = self.geometry.metrics;
        let Some((rect, items)) = &self.menu else {
            return;
        };
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
                    Paint::Chosen,
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
                selected,
                ..
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
            let text_y = palette_text_y(rect, metrics);
            // What's selected, on the selection's colour, as in a pane.
            if let Some(selected) = selected
                && let (Some(before), Some(chosen)) =
                    (text.get(..selected.start), text.get(selected.clone()))
            {
                painted.over.push(Fill::solid(
                    [
                        self.palette_text_x + mono_width(before, metrics),
                        text_y,
                        mono_width(chosen, metrics),
                        metrics.cell[1],
                    ],
                    Paint::Selected,
                ));
            }
            painted.over_texts.push(Text {
                origin: [self.palette_text_x, text_y],
                content: text,
                ink: Ink::Plain,
                font: TextFont::Mono,
            });
            for ((choice, picked), row) in choices.iter().zip(&self.choices) {
                if *picked {
                    painted.over.push(Fill::rounded(
                        row.to_array(),
                        Paint::Chosen,
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

    /// A count of workspaces or tabs hidden, in quiet text, on a soft
    /// background while the mouse is over it, as a name is.
    fn paint_hidden<'a>(&self, count: &'a HiddenCount, hovered: bool, painted: &mut Painted<'a>) {
        let metrics = self.geometry.metrics;
        let rect = count.rect;
        let ink = if hovered { Ink::Plain } else { Ink::Muted };
        match count.bar {
            Bar::Top => {
                if hovered {
                    painted.fills.push(Fill::rounded(
                        rect.to_array(),
                        Paint::Hover,
                        (1.5 * metrics.padding).round(),
                    ));
                }
                painted.texts.push(Text {
                    origin: [
                        count.text_x,
                        (rect.y + (rect.height - metrics.ui_line) / 2.0).round(),
                    ],
                    content: &count.text,
                    ink,
                    font: TextFont::Ui,
                });
            }
            Bar::Bottom if self.bottom_shown => {
                if hovered {
                    self.push_bottom_fill(rect, Paint::Hover, &mut painted.fills);
                }
                painted.texts.push(Text {
                    origin: [
                        count.text_x,
                        (rect.y + (rect.height - metrics.cell[1]) / 2.0).round(),
                    ],
                    content: &count.text,
                    ink,
                    font: TextFont::Mono,
                });
            }
            Bar::Bottom => {}
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

/// The counts of names hidden at the ends of the `strips`, scrolled to
/// `scrolls` now, and where a click on each scrolls them to.
fn count_hidden(
    strips: [(Bar, &strip::Strip); 2],
    scrolls: Scrolls,
    metrics: Metrics,
    measure: &mut dyn FnMut(&str, bool) -> f32,
) -> (Vec<HiddenCount>, Vec<(Hit, Scrolls)>) {
    let mut hidden = Vec::new();
    let mut scroll_to = Vec::new();
    for (bar, strip) in strips {
        for end in [End::Before, End::After] {
            let Some(count) = strip.hidden(end) else {
                continue;
            };
            let text = match end {
                End::Before => format!("‹{}", count.count),
                End::After => format!("{}›", count.count),
            };
            let to = match bar {
                Bar::Top => Scrolls {
                    workspaces: count.to,
                    ..scrolls
                },
                Bar::Bottom => Scrolls {
                    tabs: count.to,
                    ..scrolls
                },
            };
            scroll_to.push((Hit::Hidden(bar, end), to));
            let width = match bar {
                Bar::Top => measure(&text, false),
                Bar::Bottom => mono_width(&text, metrics),
            };
            hidden.push(HiddenCount {
                bar,
                end,
                text_x: (count.rect.x + (count.rect.width - width) / 2.0).round(),
                text,
                rect: count.rect,
            });
        }
    }
    (hidden, scroll_to)
}

/// What the bottom bar holds, and where.
#[derive(Default)]
struct Bottom {
    mode: Option<(ModeName, Rect)>,
    waiting: Option<(String, Rect)>,
    tabs: Vec<Item<TabId>>,
    new_tab: Option<Rect>,
    notes: Vec<Note>,
    /// The tabs' strip, and where it's scrolled to now.
    strip: strip::Strip,
    scroll: Scroll,
    /// The note, if it's cut short.
    cut: Option<Cut>,
}

/// How wide `text` is in the terminal's font, a cell per character, or
/// two for a wide one.
fn mono_width(text: &str, metrics: Metrics) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let cells = crate::render::mono_width(text) as f32;
    cells * metrics.cell[0]
}

/// The least room kept for the note when the tabs overflow, in cells, and
/// the share of the bar it's kept at between, so messages always have
/// somewhere to go.
const NOTE_ROOM: (f32, f32, f32) = (16.0, 0.25, 48.0);

/// Lays out the bottom bar, as `status` has it, in the terminal's font, a
/// cell per character: the mode at its left, and after it the keys
/// `waiting` for the rest of a binding, if any, written short; then the
/// `tabs`, scrolled from `scroll`, and if `shown`, a `+`, and at its right
/// the note, how many panes the tab has, and a hint. While something's
/// typed, what's under it, if anything.
///
/// The tabs come first, with all the bar if they need it. When there are
/// more than fit, their strip ends short of the bar's end, keeping room for
/// the note however long or short it is, so they never move as messages
/// come and go; in a bar too narrow for that, the tab shown shows alone.
/// The note takes the room the tabs leave: where it needs more, the hint
/// goes first, then the panes, then it's cut short. While keys wait, the
/// tabs make room for them after the mode, and their scroll isn't kept
/// from then, so they're as they were once the keys are done.
#[allow(clippy::too_many_lines)]
fn lay_out_bottom(
    geometry: &Geometry,
    (status, waiting): (&Status, Option<&str>),
    tabs: &[(TabId, String, bool)],
    shown: bool,
    scroll: Scroll,
) -> Bottom {
    // While something's typed, the mode's hint, of how to type, says
    // nothing new.
    let typing = matches!(status, Status::Typing { .. });
    let &Status::Tabs {
        mode,
        ref note,
        kind,
        panes,
    } = status.bar()
    else {
        return Bottom {
            scroll,
            ..Bottom::default()
        };
    };
    let metrics = geometry.metrics;
    let padding = metrics.padding;
    let bottom = geometry.bottom;
    let mono = |text: &str| mono_width(text, metrics);
    // As wide as the widest of the ordinary names, whichever is shown, so
    // nothing after it moves as the mode changes; wider only for a longer
    // name, while it's shown.
    let ordinary = (ModeName::ORDINARY.iter())
        .map(|mode| mono(mode.text()))
        .fold(0.0, f32::max);
    let mode = mode.map(|mode| {
        let rect = Rect {
            x: 0.0,
            y: bottom.y,
            width: (ordinary.max(mono(mode.text())) + 6.0 * padding).round(),
            height: bottom.height,
        };
        (mode, rect)
    });
    let panes = match panes {
        0 => None,
        1 => Some("1 pane".to_owned()),
        panes => Some(format!("{panes} panes")),
    };
    let hint = (mode.filter(|_| !typing)).map(|(mode, _)| mode.hint().to_owned());
    let mode_end = mode.map_or(0.0, |(_, rect)| rect.x + rect.width);
    let waiting = waiting.map(|waiting| {
        let rect = Rect {
            x: mode_end,
            y: bottom.y,
            width: (mono(waiting) + 6.0 * padding).round(),
            height: bottom.height,
        };
        (waiting.to_owned(), rect)
    });
    let tabs_start = waiting
        .as_ref()
        .map_or(mode_end, |(_, rect)| rect.x + rect.width);
    let right = bottom.width - 3.0 * padding;
    let gap = 4.0 * padding;
    let plus = if shown { bottom.height } else { 0.0 };
    let cell = metrics.cell[0];
    let full = (tabs_start, (right - plus).max(tabs_start));
    let tab_width = |text: &str| (mono(text) + 6.0 * padding).round();
    let mut width = |index: usize| tab_width(&tabs[index].1);
    let digits = tabs.len().to_string().len();
    #[allow(clippy::cast_precision_loss)]
    let count_width = ((digits + 1) as f32 * cell + 3.0 * padding).round();
    let least = 8.0 * cell;
    let selected = (tabs.iter())
        .position(|(_, _, active)| *active)
        .map(|index| (index, tabs[index].0.0));
    // The room the note keeps from tabs that overflow, whatever it says:
    // a share of the bar, within bounds, but never so much the counts and
    // the tab shown can't fit whole.
    let (fewest, share, most) = NOTE_ROOM;
    let span = if strip::fits_all(tabs.len(), &mut width, full, 0.0) {
        full
    } else {
        let kept = (share * bottom.width).clamp(fewest * cell, most * cell);
        let shown = selected.map_or(least, |(index, _)| width(index).max(least));
        let end = (full.1 - kept).max(tabs_start + 2.0 * count_width + shown);
        (tabs_start, end.min(full.1))
    };
    let room = strip::Room {
        span,
        y: bottom.y,
        height: bottom.height,
        spacing: 0.0,
        count_width,
        least,
        keep_before: true,
    };
    let strip = strip::lay_out(tabs.len(), selected, &mut width, room, scroll);
    let items = (strip.shown.iter())
        .map(|&(index, rect)| {
            let (id, text, active) = &tabs[index];
            // A tab too wide for the strip alone is cut short to it.
            let text = if rect.width < tab_width(text) {
                cut(text, cells_in(rect.width - 6.0 * padding, cell))
            } else {
                text.clone()
            };
            Item {
                id: *id,
                text,
                detail: None,
                active: *active,
                rect,
                mark: None,
            }
        })
        .collect();
    let new_tab = shown.then_some(Rect {
        x: strip.end,
        y: bottom.y,
        width: bottom.height,
        height: bottom.height,
    });
    // The note, the panes and the hint, from the right, in the room left.
    let zone = (strip.end + plus + gap).min(right);
    let ink = match kind {
        NoteKind::Label => Ink::Muted,
        NoteKind::Message => Ink::Plain,
        NoteKind::Warning { .. } => Ink::Warning,
    };
    let mut wanted = vec![(note.clone(), ink), (panes, Ink::Muted), (hint, Ink::Muted)];
    let width = |wanted: &[(Option<String>, Ink)]| {
        let texts = wanted.iter().filter_map(|(text, _)| text.as_deref());
        let (count, total) = texts.fold((0.0, 0.0), |(count, total), text| {
            (count + 1.0, total + mono(text))
        });
        total + (count - 1.0_f32).max(0.0) * gap
    };
    while wanted.len() > 1 && zone + width(&wanted) > right {
        wanted.pop();
    }
    let mut cut_note = None;
    if let [(Some(text), _)] = wanted.as_mut_slice()
        && zone + mono(text) > right
    {
        let cells = cells_in(right - zone, cell);
        // A word or two cut to a letter says nothing: `…` alone says
        // there's more, under the mouse.
        let short = if cells < 6 {
            "…".to_owned()
        } else {
            cut(text, cells)
        };
        let whole = std::mem::replace(text, short);
        if cells < 1 {
            wanted.clear();
        } else {
            let rect = Rect {
                x: zone,
                y: bottom.y,
                width: right - zone,
                height: bottom.height,
            };
            cut_note = Some((rect, whole));
        }
    }
    let mut notes = Vec::new();
    let mut x = right;
    for (text, ink) in wanted.into_iter().rev() {
        let Some(text) = text else { continue };
        x = (x - mono(&text)).round();
        notes.push(Note { text, x, ink });
        x -= gap;
    }
    notes.reverse();
    let cut = cut_note.map(|(rect, whole)| Cut {
        rect,
        whole,
        warning: matches!(kind, NoteKind::Warning { .. }),
        raised: std::cell::OnceCell::new(),
    });
    Bottom {
        scroll: if waiting.is_some() {
            scroll
        } else {
            strip.scroll
        },
        mode,
        waiting,
        tabs: items,
        new_tab,
        notes,
        strip,
        cut,
    }
}

/// `text`, cut short to fit `width` in the interface font, bold if `bold`,
/// as `measure` says, ending with `…` if it's cut.
fn cut_to(
    text: &str,
    width: f32,
    bold: bool,
    measure: &mut dyn FnMut(&str, bool) -> f32,
) -> String {
    if measure(text, bold) <= width.ceil() {
        return text.to_owned();
    }
    let mut kept: String = text.to_owned();
    while kept.pop().is_some() {
        let cut = format!("{}…", kept.trim_end());
        if measure(&cut, bold) <= width {
            return cut;
        }
    }
    "…".to_owned()
}

/// How many whole cells `width` holds, `cell` wide each.
fn cells_in(width: f32, cell: f32) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cells = (width / cell).max(0.0).floor() as usize;
    cells
}

/// `text`, cut short to `cells` cells of the terminal's font, ending with
/// `…` if it's cut.
fn cut(text: &str, cells: usize) -> String {
    if crate::render::mono_width(text) <= cells {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = crate::render::mono_width(ch.encode_utf8(&mut [0; 4]));
        if used + width + 1 > cells {
            break;
        }
        out.push(ch);
        used += width;
    }
    let kept = out.trim_end().len();
    out.truncate(kept);
    out.push('…');
    out
}

/// `text` broken into lines of at most `cells` cells of the terminal's
/// font, between words where it can be, and at its own line breaks.
fn wrap(text: &str, cells: usize) -> Vec<String> {
    let cells = cells.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for word in paragraph.split(' ') {
            let width = crate::render::mono_width(word);
            let space = usize::from(!line.is_empty());
            if used + space + width <= cells {
                if space == 1 {
                    line.push(' ');
                }
                line.push_str(word);
                used += space + width;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            // A word longer than a line is broken where the line ends.
            for ch in word.chars() {
                let width = crate::render::mono_width(ch.encode_utf8(&mut [0; 4]));
                if used + width > cells {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push(ch);
                used += width;
            }
        }
        lines.push(line);
    }
    lines
}

/// A box raised above the bottom bar holding `texts`, each wrapped to fit
/// the window: starting at `x`, or as near as it can, or else ending at
/// the bar's right. Where there are more lines than fit above the bar,
/// the last are kept.
fn raise(geometry: &Geometry, texts: &[String], x: Option<f32>, warning: bool) -> Raised {
    let metrics = geometry.metrics;
    let padding = metrics.padding;
    let [cell_width, cell_height] = metrics.cell;
    let [width, _] = geometry.size;
    let cells = cells_in((width - 8.0 * padding).max(cell_width), cell_width);
    let fit = cells_in(
        geometry.bottom.y - geometry.top.height - 8.0 * padding,
        cell_height,
    )
    .max(1);
    // From the last, wrapping only as many as fit.
    let mut lines: Vec<String> = Vec::new();
    for text in texts.iter().rev() {
        if lines.len() >= fit {
            break;
        }
        let mut wrapped = wrap(text, cells);
        wrapped.append(&mut lines);
        lines = wrapped;
    }
    if lines.len() > fit {
        lines.drain(..lines.len() - fit);
    }
    let widest = (lines.iter())
        .map(|line| mono_width(line, metrics))
        .fold(0.0, f32::max);
    #[allow(clippy::cast_precision_loss)]
    let height = (lines.len() as f32 * cell_height + 3.0 * padding).round();
    let box_width = (widest + 6.0 * padding).round();
    let last = width - 2.0 * padding - box_width;
    let x = x
        .map_or(last, |x| x - 3.0 * padding)
        .min(last)
        .max(2.0 * padding)
        .round();
    Raised {
        rect: Rect {
            x,
            y: (geometry.bottom.y - height - 2.0 * padding).round(),
            width: box_width,
            height,
        },
        lines,
        warning,
    }
}

/// Draws `raised` over the panes, as the palette is, or for something gone
/// wrong, as an alert is, its lines in the terminal's font.
fn paint_raised<'a>(raised: &'a Raised, geometry: &Geometry, painted: &mut Painted<'a>) {
    let metrics = geometry.metrics;
    let padding = metrics.padding;
    let rect = raised.rect;
    push_shadow(&mut painted.over, rect, geometry.radius, padding);
    let (fill, edge, ink) = if raised.warning {
        (Paint::Warning, Paint::WarningEdge, Ink::Warning)
    } else {
        (Paint::Palette, Paint::PaletteEdge, Ink::Plain)
    };
    // A warning's wash is faint: the palette's colour under it keeps the
    // panes from showing through.
    if raised.warning {
        painted.over.push(Fill::rounded(
            rect.to_array(),
            Paint::Palette,
            geometry.radius,
        ));
    }
    painted
        .over
        .push(Fill::rounded(rect.to_array(), fill, geometry.radius).bordered(geometry.edge, edge));
    let top = rect.y + 1.5 * padding;
    for (row, line) in (0_u16..).zip(&raised.lines) {
        painted.over_texts.push(Text {
            origin: [
                rect.x + 3.0 * padding,
                (top + f32::from(row) * metrics.cell[1]).round(),
            ],
            content: line,
            ink,
            font: TextFont::Mono,
        });
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
    // A chat asking stands out, in clackatoo's colour.
    if let Some((mark, x)) = item.mark {
        painted.texts.push(Text {
            origin: [x, text_y],
            content: mark.suffix(),
            ink: match mark {
                Mark::Asking => Ink::Accent,
                Mark::Working | Mark::Unseen => Ink::Muted,
            },
            font: TextFont::Ui,
        });
    }
}

/// Lays out `workspaces` in the top bar, in `span`, as pills `height`
/// tall at `y`: each its dot, its name and what it says of its chats,
/// and quieter, how many tabs it has, unless it's named with a number, as
/// workspaces are by default: "1 3" would read as nonsense. Room is kept
/// for the widest mark whether one shows or not, so no pill moves as a
/// chat starts or stops working. Where there are more than fit, they
/// scroll from `scroll`, as tabs do. Returns those shown, and their strip.
fn lay_out_workspaces(
    workspaces: &[WorkspaceName],
    span: std::ops::Range<f32>,
    (metrics, y, height): (Metrics, f32, f32),
    measure: &mut dyn FnMut(&str, bool) -> f32,
    scroll: Scroll,
) -> (Vec<Item<WorkspaceId>>, strip::Strip) {
    let padding = metrics.padding;
    let dot = metrics.dot();
    let room = (Mark::ALL.iter())
        .map(|mark| measure(mark.suffix(), true))
        .fold(0.0, f32::max);
    let count_width = (measure(&format!("‹{}", workspaces.len()), false) + 4.0 * padding).round();
    let selected = (workspaces.iter())
        .position(|workspace| workspace.shown)
        .map(|index| (index, workspaces[index].id.0));
    // A pill's width, besides its name's.
    let around = |workspace: &WorkspaceName, measure: &mut dyn FnMut(&str, bool) -> f32| {
        let detail =
            (workspace.tabs_said()).map_or(0.0, |detail| 1.5 * padding + measure(&detail, false));
        room + detail + dot + 8.0 * padding
    };
    let strip_room = strip::Room {
        span: (span.start, span.end),
        y,
        height,
        spacing: padding,
        count_width,
        least: 4.0 * height,
        keep_before: false,
    };
    let strip = {
        let mut width = |index: usize| {
            let workspace = &workspaces[index];
            let name = measure(&workspace.name, workspace.shown);
            (name + around(workspace, measure)).round()
        };
        strip::lay_out(workspaces.len(), selected, &mut width, strip_room, scroll)
    };
    let placed = (strip.shown.iter())
        .map(|&(index, rect)| {
            let workspace = &workspaces[index];
            let active = workspace.shown;
            // A pill too wide for the strip alone has its name cut short.
            let text = cut_to(
                &workspace.name,
                rect.width - around(workspace, measure),
                active,
                measure,
            );
            // Each tab count ends its pill, three paddings in.
            let detail = workspace.tabs_said().map(|detail| {
                let x = (rect.x + rect.width - 3.0 * padding - measure(&detail, false)).round();
                (detail, x)
            });
            // A mark goes straight after the name, as `paint_workspace`
            // places the name.
            let text_x = (rect.x + 3.0 * padding).round() + dot + 2.0 * padding;
            let after = text_x + measure(&text, active);
            Item {
                id: workspace.id,
                text,
                detail,
                active,
                rect,
                mark: workspace.mark.map(|mark| (mark, after.round())),
            }
        })
        .collect();
    (placed, strip)
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

/// The pill over `tile`'s bottom right that says how far its terminal is
/// scrolled back, `pill`, as [`Pane::pill`](crate::pane::Pane::pill) words
/// it: raised as the palette is, its distance in the text colour and the
/// way back muted after it, in the terminal's font, a cell of room either
/// side and a cell from the pane's right edge.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn paint_pill<'a>(
    geometry: &Geometry,
    tile: &Tile,
    pill: &'a str,
) -> (Fill, Vec<Text<'a>>) {
    let metrics = geometry.metrics;
    let [cell_width, cell_height] = metrics.cell;
    let width = ((crate::render::mono_width(pill) + 2) as f32 * cell_width).round();
    let height = (cell_height + metrics.padding).round();
    let rect = tile.rect;
    let x = (rect.x + rect.width - width - cell_width).round();
    let y = (rect.y + rect.height - height - metrics.padding).round();
    let fill = Fill::rounded([x, y, width, height], Paint::Palette, geometry.radius)
        .bordered(geometry.edge, Paint::PaletteEdge);
    let top = (y + (height - cell_height) / 2.0).round();
    let left = x + cell_width;
    let far = pill.split_once(" · ").map_or(pill, |(far, _)| far);
    let back = &pill[far.len()..];
    let mut texts = vec![Text {
        origin: [left, top],
        content: far,
        ink: Ink::Plain,
        font: TextFont::Mono,
    }];
    if !back.is_empty() {
        texts.push(Text {
            origin: [
                left + crate::render::mono_width(far) as f32 * cell_width,
                top,
            ],
            content: back,
            ink: Ink::Muted,
            font: TextFont::Mono,
        });
    }
    (fill, texts)
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
    // Beside a completion menu, drawn into the cell left free between
    // them, as far as leaves the gap, or not at all if the cell's
    // narrower than the gap.
    let toward = (cell_width - crate::editor::BESIDE_GAP * metrics.padding).max(0.0);
    let nudge = match view.beside {
        Some(crate::editor::Beside::Right) => -toward,
        Some(crate::editor::Beside::Left) => toward,
        None => 0.0,
    };
    // A cell of room either side of the text, and half a cell above and
    // below.
    let rect = Rect {
        x: (origin[0] + placed.left as f32 * cell_width + nudge).round(),
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
        if line.rule {
            over.push(Fill::solid(
                [
                    left,
                    (y + (cell_height - geometry.edge) / 2.0).round(),
                    placed.width as f32 * cell_width,
                    geometry.edge,
                ],
                Paint::PaletteEdge,
            ));
        }
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
                Paint::Chosen,
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
    /// The count of workspaces or tabs hidden at one end of their strip,
    /// which scrolls it that way.
    Hidden(Bar, End),
    /// The bottom bar's note, cut short, which shows whole while the mouse
    /// is over it.
    Note,
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
    use crate::keymap::WhichKeys;

    /// Cells 10 × 20, interface text 16 high, and a padding of 4: names in
    /// the top bar are 24 high, the top bar 36 and the bottom bar 26.
    const METRICS: Metrics = Metrics {
        cell: [10.0, 20.0],
        ui_line: 16.0,
        padding: 4.0,
    };

    #[test]
    // Whole numbers, compared exactly.
    #[allow(clippy::float_cmp)]
    fn lines_between_panes_grow_with_the_screens_scale_in_whole_pixels() {
        // The padding is 4 points, in pixels at 1×, 1.25×, 1.5× and 2×.
        for (padding, line) in [(4.0, 1.0), (5.0, 1.0), (6.0, 2.0), (8.0, 2.0)] {
            let metrics = Metrics { padding, ..METRICS };
            let geometry = Geometry::new([800, 600], metrics);
            assert_eq!(geometry.edge, line, "a padding of {padding}");
            assert_eq!(geometry.area.gap, line);
        }
    }

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
                    WorkspaceName::plain(1, "clackatoo", 2, true),
                    WorkspaceName::plain(2, "peios", 1, false),
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
                scrolls: Scrolls::default(),
                messages: None,
                waiting: None,
                which_keys: None,
            },
            &mut measure,
        )
    }

    #[test]
    fn an_alert_goes_before_the_buttons_and_the_workspaces_make_room() {
        let buttons = ButtonLayout::default();
        let workspaces: Vec<_> = (1..=12)
            .map(|id| WorkspaceName::plain(id, &format!("workspace {id}"), 1, id == 1))
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
                scrolls: Scrolls::default(),
                messages: None,
                waiting: None,
                which_keys: None,
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
    fn the_mode_keeps_its_width_but_for_visual_mode_from_insert_mode() {
        let width = |mode| {
            let status = Status::Tabs {
                mode: Some(mode),
                note: None,
                kind: NoteKind::Label,
                panes: 1,
            };
            chrome(status, &ButtonLayout::default())
                .mode
                .unwrap()
                .1
                .width
        };
        for mode in ModeName::ORDINARY {
            assert_eq!(width(mode), width(ModeName::Normal), "{mode:?}");
        }
        // The names of visual mode from insert mode widen it, while shown.
        for mode in [ModeName::InsertVisual, ModeName::InsertVisualLine] {
            assert_eq!(width(mode), 15.0 * 10.0 + 24.0, "{mode:?}");
        }
    }

    #[test]
    fn what_is_raised_over_the_panes_is_known() {
        let status = |note: &str| Status::Tabs {
            mode: Some(ModeName::Normal),
            note: Some(note.into()),
            kind: NoteKind::Label,
            panes: 1,
        };
        let mut plain = chrome(status("short"), &ButtonLayout::default());
        assert!(!plain.raises_over_panes(Hit::Nothing));
        plain.palette = Some(Rect::default());
        assert!(plain.raises_over_panes(Hit::Nothing));
        // A note cut short is raised whole while the mouse is over it.
        let long = chrome(status(&"long ".repeat(200)), &ButtonLayout::default());
        assert!(long.cut.is_some());
        assert!(!long.raises_over_panes(Hit::Nothing));
        assert!(long.raises_over_panes(Hit::Note));
    }

    #[test]
    fn points_hit_what_they_are_over() {
        let chrome = chrome(
            Status::Tabs {
                mode: Some(ModeName::Normal),
                note: None,
                kind: NoteKind::Label,
                panes: 2,
            },
            &ButtonLayout::default(),
        );
        // clackatoo's pill starts two paddings in, 8, and holds a dot of 6,
        // 9 characters, room of 16 for a mark, its 1 character of tabs 6
        // after them, and 32 of padding: 140 wide.
        assert_eq!(chrome.workspaces[0].rect.x, 8.0);
        assert_eq!(chrome.workspaces[0].rect.width, 140.0);
        let (count, count_x) = chrome.workspaces[0].detail.clone().unwrap();
        assert_eq!((count.as_str(), count_x), ("2", 8.0 + 140.0 - 12.0 - 8.0));
        assert_eq!(
            chrome.hit([20.0, 18.0], false),
            Hit::Workspace(WorkspaceId(1))
        );
        assert_eq!(
            chrome.hit([160.0, 18.0], false),
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
        let notes: Vec<&str> = chrome.notes.iter().map(|note| note.text.as_str()).collect();
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
                    WorkspaceName::plain(1, "1", 3, true),
                    WorkspaceName::plain(2, "work 2", 3, false),
                    // Marked, and numbered still.
                    WorkspaceName {
                        mark: Some(Mark::Asking),
                        ..WorkspaceName::plain(3, "3", 2, false)
                    },
                    WorkspaceName {
                        mark: Some(Mark::Unseen),
                        ..WorkspaceName::plain(4, "peios", 2, false)
                    },
                ],
                tabs: Vec::new(),
                status: Status::tabs(),
                buttons: &buttons,
                bottom_shown: true,
                menu: None,
                alert: None,
                rounded: false,
                finder: None,
                scrolls: Scrolls::default(),
                messages: None,
                waiting: None,
                which_keys: None,
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
        // What a workspace says of its chats goes after its name.
        let [.., three, peios] = &chrome.workspaces[..] else {
            panic!("four workspaces");
        };
        assert_eq!((three.text.as_str(), three.detail.as_ref()), ("3", None));
        let mark = three.mark.expect("its mark");
        assert_eq!(mark.0, Mark::Asking);
        // After the dot and the name, "3", 8 wide.
        let padding = METRICS.padding;
        let name = three.rect.x + 3.0 * padding + METRICS.dot() + 2.0 * padding;
        assert!(
            (mark.1 - (name + 8.0)).abs() <= 1.0,
            "{mark:?} {:?}",
            three.rect
        );
        assert_eq!(peios.mark.map(|(mark, _)| mark), Some(Mark::Unseen));
        assert!(peios.detail.is_some());
    }

    #[test]
    fn a_workspaces_pill_keeps_its_place_and_width_as_its_mark_comes_and_goes() {
        let buttons = ButtonLayout::default();
        let lay = |mark: Option<Mark>, width: u32| {
            let workspaces = (1..=6)
                .map(|id| WorkspaceName {
                    mark: (id == 2).then_some(mark).flatten(),
                    ..WorkspaceName::plain(id, &format!("workspace {id}"), 2, id == 1)
                })
                .collect();
            let contents = Contents {
                workspaces,
                tabs: Vec::new(),
                status: Status::tabs(),
                buttons: &buttons,
                bottom_shown: true,
                menu: None,
                alert: None,
                rounded: false,
                finder: None,
                scrolls: Scrolls::default(),
                messages: None,
                waiting: None,
                which_keys: None,
            };
            let chrome = Chrome::new(Geometry::new([width, 600], METRICS), contents, &mut measure);
            (chrome.workspaces.iter())
                .map(|item| item.rect)
                .collect::<Vec<_>>()
        };
        for width in [1600, 900] {
            let plain = lay(None, width);
            for mark in Mark::ALL {
                assert_eq!(lay(Some(mark), width), plain, "{mark:?} at {width}");
            }
        }
        // Narrow, some are left out, the same ones with a mark or not.
        assert!(lay(None, 900).len() < 6);
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
                selected: None,
                under: None,
            },
            &ButtonLayout::default(),
        );
        assert!(chrome.tabs.is_empty());
        let palette = chrome.palette.expect("a palette");
        assert_eq!(palette.x + palette.width / 2.0, 400.0);
        assert_eq!(palette.y + palette.height / 2.0, 300.0);
    }

    #[test]
    fn under_what_is_typed_the_bar_keeps_the_tabs_and_a_notice_but_no_hint() {
        let under = |kind| {
            Some(Box::new(Status::Tabs {
                mode: Some(ModeName::Normal),
                note: Some("gone wrong".into()),
                kind,
                panes: 1,
            }))
        };
        let typing = |kind| Status::Typing {
            label: None,
            text: ":".into(),
            choices: Vec::new(),
            selected: None,
            under: under(kind),
        };
        let typed = chrome(typing(NoteKind::Label), &ButtonLayout::default());
        assert_eq!(typed.tabs.len(), 2);
        assert!(typed.mode.is_some());
        let notes: Vec<&str> = typed.notes.iter().map(|note| note.text.as_str()).collect();
        assert!(!notes.contains(&": command"), "{notes:?}");
        let warned = chrome(
            typing(NoteKind::Warning { notice: true }),
            &ButtonLayout::default(),
        );
        assert!(warned.notice);
        // With nothing typed, the hint is there.
        let tabs = chrome(*under(NoteKind::Label).unwrap(), &ButtonLayout::default());
        let notes: Vec<&str> = tabs.notes.iter().map(|note| note.text.as_str()).collect();
        assert!(notes.contains(&": command"), "{notes:?}");
    }

    /// A message longer than any window is wide.
    const LONG: &str = "the daemon couldn't do what was asked, and here is a long account \
                        of why, so long that no bar has room for all of it at once";

    /// A window `width` wide with `count` tabs with long names, the
    /// `selected`th shown, and `note` of `kind` in the bar, its strips
    /// scrolled from `scrolls`.
    fn tabbed(
        width: u32,
        (count, selected): (u32, u32),
        (note, kind): (Option<&str>, NoteKind),
        scrolls: Scrolls,
    ) -> Chrome {
        tabbed_waiting(width, (count, selected), (note, kind), scrolls, None)
    }

    /// `tabbed`, with keys waiting for the rest of a binding, if `which`
    /// says so.
    fn tabbed_waiting(
        width: u32,
        (count, selected): (u32, u32),
        (note, kind): (Option<&str>, NoteKind),
        scrolls: Scrolls,
        which: Option<WhichKeys>,
    ) -> Chrome {
        let buttons = ButtonLayout::default();
        let tabs = (1..=count)
            .map(|id| {
                (
                    TabId(id),
                    format!("{id} a long running command"),
                    id == selected,
                )
            })
            .collect();
        Chrome::new(
            Geometry::new([width, 600], METRICS),
            Contents {
                workspaces: vec![WorkspaceName::plain(1, "clackatoo", 2, true)],
                tabs,
                status: Status::Tabs {
                    mode: Some(ModeName::Normal),
                    note: note.map(str::to_owned),
                    kind,
                    panes: 2,
                },
                buttons: &buttons,
                bottom_shown: true,
                menu: None,
                alert: None,
                rounded: false,
                finder: None,
                scrolls,
                messages: None,
                waiting: which.as_ref().map(|_| "^B".to_owned()),
                which_keys: which,
            },
            &mut measure,
        )
    }

    /// The tabs shown, by ID, and where.
    fn tab_rects(chrome: &Chrome) -> Vec<(TabId, Rect)> {
        chrome.tabs.iter().map(|tab| (tab.id, tab.rect)).collect()
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn a_message_never_covers_the_tabs_at_any_width() {
        let notes = [
            (Some(LONG), NoteKind::Message),
            (Some(LONG), NoteKind::Warning { notice: true }),
            (Some("not found"), NoteKind::Message),
            (None, NoteKind::Label),
        ];
        for width in (300..=2400).step_by(7) {
            for count in [1, 4, 15, 40] {
                for note in notes {
                    let chrome = tabbed(width, (count, count.min(8)), note, Scrolls::default());
                    let plus = chrome.new_tab.expect("a +");
                    let mut ends: Vec<f32> = (chrome.tabs.iter())
                        .map(|tab| tab.rect.x + tab.rect.width)
                        .collect();
                    ends.push(plus.x + plus.width);
                    ends.extend(
                        (chrome.hidden.iter())
                            .filter(|count| count.bar == Bar::Bottom)
                            .map(|count| count.rect.x + count.rect.width),
                    );
                    let mode = chrome.mode.expect("a mode").1;
                    for (index, text) in chrome.notes.iter().enumerate() {
                        let at = format!("{width} px, {count} tabs, {note:?}, note {index}");
                        assert!(text.x >= mode.x + mode.width, "{at}");
                        for end in &ends {
                            assert!(text.x >= *end, "{at}: {} over {end}", text.x);
                        }
                        // And it ends inside the window.
                        let right = text.x + mono_width(&text.text, METRICS);
                        assert!(right <= width as f32, "{at}");
                    }
                    // The tab shown is always in view, cut short if it must
                    // be.
                    let active = chrome.tabs.iter().find(|tab| tab.active);
                    let at = format!("{width} px, {count} tabs, {note:?}");
                    let active = active.unwrap_or_else(|| panic!("{at}: no tab shown"));
                    assert!(active.rect.width > 0.0, "{at}");
                    // Tabs that fit the bar all show, keeping no room for a
                    // note: each is 26 characters and their padding.
                    let room = width as f32 - mode.width - 26.0 - 12.0;
                    let needed = count as f32 * (26.0 * 10.0 + 24.0);
                    if needed <= room {
                        assert_eq!(chrome.tabs.len(), count as usize, "{width} px");
                    }
                }
            }
        }
    }

    #[test]
    fn tabs_that_dont_fit_count_those_hidden_and_follow_the_one_shown() {
        let chrome = tabbed(800, (15, 8), (None, NoteKind::Label), Scrolls::default());
        let shown: Vec<u32> = chrome.tabs.iter().map(|tab| tab.id.0).collect();
        assert!(shown.contains(&8), "{shown:?}");
        assert_eq!(
            *shown.last().unwrap(),
            8,
            "scrolled no further than it must"
        );
        let texts: Vec<&str> = (chrome.hidden.iter())
            .filter(|count| count.bar == Bar::Bottom)
            .map(|count| count.text.as_str())
            .collect();
        let before = shown[0] - 1;
        let after = 15 - 8;
        assert_eq!(texts, [format!("‹{before}"), format!("{after}›")]);
        // The counts are clickable, and say where they scroll to.
        let after = chrome.hidden.iter().find(|count| count.end == End::After);
        let rect = after.unwrap().rect;
        let hit = chrome.hit([rect.x + 2.0, rect.y + 2.0], false);
        assert_eq!(hit, Hit::Hidden(Bar::Bottom, End::After));
        assert!(chrome.scrolled(hit).is_some());
        assert_eq!(chrome.scrolled(Hit::NewTab), None);
    }

    #[test]
    fn clicking_a_tab_shown_moves_no_tab() {
        let first = tabbed(800, (15, 8), (None, NoteKind::Label), Scrolls::default());
        for tab in &first.tabs {
            let clicked = tabbed(800, (15, tab.id.0), (None, NoteKind::Label), first.scrolls);
            assert_eq!(tab_rects(&clicked), tab_rects(&first), "{:?}", tab.id);
            // And drawn again and again, it holds still.
            let again = tabbed(
                800,
                (15, tab.id.0),
                (None, NoteKind::Label),
                clicked.scrolls,
            );
            assert_eq!(tab_rects(&again), tab_rects(&first));
        }
    }

    #[test]
    fn a_count_clicked_scrolls_the_tabs_and_they_stay_until_another_is_picked() {
        let first = tabbed(800, (15, 1), (None, NoteKind::Label), Scrolls::default());
        assert_eq!(first.tabs[0].id, TabId(1));
        let on = first
            .scrolled(Hit::Hidden(Bar::Bottom, End::After))
            .expect("tabs after");
        let scrolled = tabbed(800, (15, 1), (None, NoteKind::Label), on);
        let past = first.tabs.last().unwrap().id.0;
        assert_eq!(scrolled.tabs[0].id.0, past + 1, "a strip's length on");
        assert!(!scrolled.tabs.iter().any(|tab| tab.active));
        // It holds there, frame after frame, and as messages come and go.
        let held = tabbed(
            800,
            (15, 1),
            (Some(LONG), NoteKind::Message),
            scrolled.scrolls,
        );
        assert_eq!(tab_rects(&held), tab_rects(&scrolled));
        // And back.
        let back = held
            .scrolled(Hit::Hidden(Bar::Bottom, End::Before))
            .expect("tabs before");
        let back = tabbed(800, (15, 1), (None, NoteKind::Label), back);
        assert_eq!(tab_rects(&back), tab_rects(&first));
        // Picking another brings it into view.
        let picked = tabbed(800, (15, 14), (None, NoteKind::Label), scrolled.scrolls);
        assert!(picked.tabs.iter().any(|tab| tab.active));
    }

    #[test]
    fn overflowing_tabs_hold_still_as_messages_come_and_go() {
        let plain = tabbed(1000, (15, 8), (None, NoteKind::Label), Scrolls::default());
        for note in [
            (Some(LONG), NoteKind::Message),
            (Some("not found"), NoteKind::Message),
            (Some(LONG), NoteKind::Warning { notice: false }),
            (Some("↑42"), NoteKind::Label),
        ] {
            let noted = tabbed(1000, (15, 8), note, plain.scrolls);
            assert_eq!(tab_rects(&noted), tab_rects(&plain), "{note:?}");
            assert_eq!(noted.new_tab, plain.new_tab);
        }
    }

    /// What Ctrl+B takes, as the window's own keys say it.
    fn ctrl_b() -> WhichKeys {
        let rows = [
            ("1-9", "go to tab"),
            ("c", "new tab"),
            ("[", "tab before"),
            ("]", "next tab"),
            (",", "rename tab"),
            ("p", "+pick a pane"),
            ("h", "pane left"),
            ("j", "pane below"),
            ("k", "pane above"),
            ("l", "pane right"),
            ("|", "split right"),
            ("-", "split below"),
            ("x", "close pane"),
            ("o", "jump back"),
            ("i", "jump forward"),
            ("a", "agent chat"),
            ("w", "workspaces"),
            ("r", "connect"),
            ("^B", "send Ctrl+B"),
        ];
        WhichKeys {
            title: "Ctrl+B".into(),
            rows: (rows.iter())
                .map(|&(keys, does)| (keys.to_owned(), does.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn the_keys_waiting_are_marked_by_the_mode_and_keep_the_tabs_scroll() {
        let plain = tabbed(1000, (15, 8), (None, NoteKind::Label), Scrolls::default());
        let waiting = tabbed_waiting(
            1000,
            (15, 8),
            (None, NoteKind::Label),
            plain.scrolls,
            Some(ctrl_b()),
        );
        let mode = waiting.mode.expect("a mode").1;
        let (mark, rect) = waiting.waiting.clone().expect("the mark");
        assert_eq!(mark, "^B");
        assert_eq!(rect.x, mode.x + mode.width, "just after the mode");
        let first = waiting.tabs.first().expect("tabs").rect;
        assert!(first.x >= rect.x + rect.width, "the tabs after it");
        assert_eq!(waiting.scrolls, plain.scrolls, "kept for when they go");
        // The bar's right is as it was: the keys are in a panel.
        assert_eq!(waiting.notes, plain.notes);
        assert!(plain.waiting.is_none() && plain.keys.is_none());
    }

    #[test]
    fn rows_too_many_to_fit_above_the_bar_take_more_columns_then_say_how_many_more() {
        // Too short for them all at 2×: as many columns as fit, and a
        // last row saying how many more there are, none left out unsaid.
        let double = Metrics {
            cell: [20.0, 40.0],
            ui_line: 32.0,
            padding: 8.0,
        };
        for height in [400, 500, 700, 1400] {
            let chrome = Chrome::new(
                Geometry::new([1800, height], double),
                Contents {
                    workspaces: Vec::new(),
                    tabs: Vec::new(),
                    status: Status::tabs(),
                    buttons: &ButtonLayout::default(),
                    bottom_shown: true,
                    menu: None,
                    alert: None,
                    rounded: false,
                    finder: None,
                    scrolls: Scrolls::default(),
                    messages: None,
                    waiting: Some("^B".into()),
                    which_keys: Some(ctrl_b()),
                },
                &mut measure,
            );
            let panel = chrome.keys.as_ref().expect("the panel");
            let rows = panel.rows();
            let at = format!("{height} px");
            let shown = match rows.last() {
                Some(("…", more)) => {
                    let more: usize = more.trim_end_matches(" more").parse().unwrap();
                    rows.len() - 1 + more
                }
                _ => rows.len(),
            };
            assert_eq!(shown, 19, "{at}: {rows:?}");
            assert!(panel.rect.y >= chrome.geometry.top.height, "{at}");
            assert!(panel.rect.x + panel.rect.width <= 1800.0, "{at}");
            // A column more fits them all; then none does.
            if height == 500 {
                assert_eq!(rows.len(), 19, "{at}");
            }
            if height == 400 {
                assert_eq!(rows.last().map(|row| row.0), Some("…"), "{at}");
            }
        }
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn the_panel_of_keys_fits_above_the_bar_at_any_width_in_up_to_three_columns() {
        for width in (300..=2400).step_by(13) {
            let chrome = tabbed_waiting(
                width,
                (4, 1),
                (None, NoteKind::Label),
                Scrolls::default(),
                Some(ctrl_b()),
            );
            let panel = chrome.keys.as_ref().expect("the panel");
            let rect = panel.rect;
            let at = format!("{width} px");
            assert!(rect.x >= 0.0 && rect.x + rect.width <= width as f32, "{at}");
            assert!(rect.y + rect.height <= chrome.geometry.bottom.y, "{at}");
            assert!(
                rect.y >= chrome.geometry.top.y + chrome.geometry.top.height,
                "{at}"
            );
            // Every key is listed while there's room for its column.
            let columns: Vec<f32> =
                panel
                    .key_origins()
                    .iter()
                    .map(|at| at[0])
                    .fold(Vec::new(), |mut columns, x| {
                        if !columns.contains(&x) {
                            columns.push(x);
                        }
                        columns
                    });
            assert!((1..=3).contains(&columns.len()), "{at}: {columns:?}");
            assert_eq!(panel.rows().len(), 19, "{at}");
            for (keys, does) in panel.rows() {
                assert!(!keys.is_empty() && !does.is_empty(), "{at}");
            }
        }
        // At a laptop's 900 points, three columns, right-aligned, with
        // every key and what it does whole.
        let chrome = tabbed_waiting(
            900,
            (4, 1),
            (None, NoteKind::Label),
            Scrolls::default(),
            Some(ctrl_b()),
        );
        let panel = chrome.keys.as_ref().unwrap();
        assert_eq!(panel.rows()[0], ("1-9", "go to tab"));
        assert_eq!(panel.rows()[18], ("^B", "send Ctrl+B"));
        assert_eq!(
            panel.rect.x + panel.rect.width,
            900.0 - 2.0 * METRICS.padding
        );
        let origins = panel.key_origins();
        assert!(origins[0][0] < origins[7][0] && origins[7][0] < origins[14][0]);
        assert_eq!(origins[0][1], origins[7][1], "side by side");
        // Its title, then its keys in bold amber and what they do quietly.
        let painted = chrome.paint(Hit::Nothing);
        let ink = |text: &str| {
            (painted.over_texts.iter())
                .find(|each| each.content == text)
                .map(|each| each.ink)
        };
        assert_eq!(ink("Ctrl+B"), Some(Ink::Bold));
        assert_eq!(ink("c"), Some(Ink::Key));
        assert_eq!(ink("new tab"), Some(Ink::Muted));
        assert!(
            painted
                .texts
                .iter()
                .any(|text| text.content == "^B" && text.ink == Ink::Key)
        );
    }

    #[test]
    fn a_message_cut_short_shows_whole_with_the_mouse_over_it() {
        let chrome = tabbed(
            800,
            (15, 8),
            (Some(LONG), NoteKind::Message),
            Scrolls::default(),
        );
        let cut = chrome.cut.clone().expect("cut short");
        let (rect, raised) = (cut.rect, cut.raised(&chrome.geometry));
        let note = &chrome.notes[0];
        assert!(note.text.ends_with('…'));
        assert!(LONG.starts_with(note.text.trim_end_matches('…')));
        assert_eq!(
            chrome.hit([rect.x + 4.0, rect.y + 4.0], false),
            Hit::Note,
            "only a note cut short is hit"
        );
        // Whole, wrapped above the bar, inside the window.
        assert_eq!(raised.lines.join(" "), LONG);
        assert!(raised.rect.y + raised.rect.height <= chrome.geometry.bottom.y);
        assert!(raised.rect.x >= 0.0 && raised.rect.x + raised.rect.width <= 800.0);
        let painted = |hover| chrome.paint(hover).over_texts.len();
        assert!(painted(Hit::Note) > painted(Hit::Nothing));
        // A short one is whole already: no box, and nothing to hit.
        let chrome = tabbed(
            800,
            (2, 1),
            (Some("not found"), NoteKind::Message),
            Scrolls::default(),
        );
        assert_eq!(chrome.cut, None);
        let note = &chrome.notes[0];
        assert_eq!(chrome.hit([note.x + 2.0, 584.0], false), Hit::Nothing);
    }

    #[test]
    fn a_warning_cut_short_shows_whole_without_the_mouse_for_a_moment() {
        let lay = |notice| {
            tabbed(
                800,
                (15, 8),
                (Some(LONG), NoteKind::Warning { notice }),
                Scrolls::default(),
            )
        };
        let (fresh, stale) = (lay(true), lay(false));
        assert_eq!(fresh.notes[0].ink, Ink::Warning);
        assert!((fresh.cut.as_ref()).is_some_and(|cut| cut.raised(&fresh.geometry).warning));
        let boxes = |chrome: &Chrome| chrome.paint(Hit::Nothing).over_texts.len();
        assert!(boxes(&fresh) > boxes(&stale));
    }

    #[test]
    fn colon_messages_lists_the_latest_that_fit_above_the_bar() {
        let buttons = ButtonLayout::default();
        let messages: Vec<String> = (1..=100)
            .map(|number| format!("message {number}"))
            .collect();
        let chrome = Chrome::new(
            Geometry::new([800, 600], METRICS),
            Contents {
                workspaces: Vec::new(),
                tabs: Vec::new(),
                status: Status::tabs(),
                buttons: &buttons,
                bottom_shown: true,
                menu: None,
                alert: None,
                rounded: false,
                finder: None,
                scrolls: Scrolls::default(),
                messages: Some(&messages),
                waiting: None,
                which_keys: None,
            },
            &mut measure,
        );
        let raised = chrome.messages.clone().expect("listed");
        assert_eq!(raised.lines.last().map(String::as_str), Some("message 100"));
        assert!(raised.lines.len() < 100, "only those that fit");
        assert!(raised.rect.y >= chrome.geometry.top.height);
        assert!(raised.rect.y + raised.rect.height <= chrome.geometry.bottom.y);
    }

    #[test]
    fn workspaces_that_dont_fit_scroll_to_the_one_shown_and_count_the_rest() {
        let buttons = ButtonLayout::default();
        let lay = |shown: u32, scrolls: Scrolls| {
            let workspaces = (1..=12)
                .map(|id| WorkspaceName::plain(id, &format!("workspace {id}"), 1, id == shown))
                .collect();
            Chrome::new(
                Geometry::new([800, 600], METRICS),
                Contents {
                    workspaces,
                    tabs: Vec::new(),
                    status: Status::tabs(),
                    buttons: &buttons,
                    bottom_shown: true,
                    menu: None,
                    alert: None,
                    rounded: false,
                    finder: None,
                    scrolls,
                    messages: None,
                    waiting: None,
                    which_keys: None,
                },
                &mut measure,
            )
        };
        let chrome = lay(11, Scrolls::default());
        assert!(chrome.workspaces.iter().any(|item| item.active));
        let before = (chrome.hidden.iter())
            .find(|count| count.bar == Bar::Top && count.end == End::Before)
            .expect("those before are counted");
        let hit = chrome.hit([before.rect.x + 2.0, before.rect.y + 2.0], false);
        assert_eq!(hit, Hit::Hidden(Bar::Top, End::Before));
        // Clicking it scrolls back; the `+` stays where it was.
        let back = lay(11, chrome.scrolled(hit).unwrap());
        assert!(!back.workspaces.iter().any(|item| item.active));
        assert_eq!(back.new_workspace, chrome.new_workspace);
        // Picking one shown moves none.
        let picked = lay(chrome.workspaces[0].id.0, chrome.scrolls);
        let rects = |chrome: &Chrome| -> Vec<Rect> {
            chrome.workspaces.iter().map(|item| item.rect).collect()
        };
        assert_eq!(rects(&picked), rects(&chrome));
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn workspaces_keep_between_the_buttons_and_off_each_other_at_any_width() {
        let buttons = ButtonLayout::default();
        for width in (200..=2400).step_by(13) {
            for (count, shown) in [(1, 1), (3, 2), (12, 1), (12, 7), (12, 12), (40, 30)] {
                let workspaces = (1..=count)
                    .map(|id| WorkspaceName::plain(id, &format!("workspace {id}"), 2, id == shown))
                    .collect();
                let chrome = Chrome::new(
                    Geometry::new([width, 600], METRICS),
                    Contents {
                        workspaces,
                        tabs: Vec::new(),
                        status: Status::tabs(),
                        buttons: &buttons,
                        bottom_shown: true,
                        menu: None,
                        // Where there's room for one beside the workspaces.
                        alert: (width >= 600).then(|| "no contact with box for 4 s".into()),
                        rounded: false,
                        finder: None,
                        scrolls: Scrolls::default(),
                        messages: None,
                        waiting: None,
                        which_keys: None,
                    },
                    &mut measure,
                );
                let first_button = (chrome.buttons.iter())
                    .map(|(_, rect)| rect.x)
                    .fold(width as f32, f32::min);
                let alert = chrome
                    .alert
                    .clone()
                    .map_or(first_button, |(_, rect)| rect.x);
                let mut rects: Vec<Rect> = chrome.workspaces.iter().map(|item| item.rect).collect();
                rects.extend(
                    (chrome.hidden.iter())
                        .filter(|count| count.bar == Bar::Top)
                        .map(|count| count.rect),
                );
                rects.extend(chrome.new_workspace);
                rects.sort_by(|a, b| a.x.total_cmp(&b.x));
                let at = format!("{width} px, {count} workspaces");
                for pair in rects.windows(2) {
                    assert!(pair[0].x + pair[0].width <= pair[1].x, "{at}: {pair:?}");
                }
                for rect in &rects {
                    assert!(rect.x >= 0.0, "{at}");
                    let end = rect.x + rect.width;
                    assert!(end <= first_button && end <= alert, "{at}");
                }
                // The workspace shown shows, cut short if it must be, where
                // there's room for any.
                if width >= 300 {
                    let active = chrome.workspaces.iter().find(|item| item.active);
                    assert!(active.is_some(), "{at}: the one shown");
                }
            }
        }
    }

    #[test]
    fn a_workspace_too_wide_for_its_strip_shows_its_name_cut_short() {
        let buttons = ButtonLayout::default();
        let long = "a workspace with a name far longer than any bar has room for";
        let chrome = Chrome::new(
            Geometry::new([400, 600], METRICS),
            Contents {
                workspaces: vec![
                    WorkspaceName::plain(1, "one", 1, false),
                    WorkspaceName::plain(2, long, 1, true),
                ],
                tabs: Vec::new(),
                status: Status::tabs(),
                buttons: &buttons,
                bottom_shown: true,
                menu: None,
                alert: None,
                rounded: false,
                finder: None,
                scrolls: Scrolls::default(),
                messages: None,
                waiting: None,
                which_keys: None,
            },
            &mut measure,
        );
        let [shown] = &chrome.workspaces[..] else {
            panic!("the one shown alone: {:?}", chrome.workspaces);
        };
        assert!(shown.active);
        assert!(shown.text.ends_with('…'), "{}", shown.text);
        assert!(long.starts_with(shown.text.trim_end_matches('…')));
        // Its tab count still ends the pill, inside it.
        let (_, count_x) = shown.detail.clone().expect("its tabs");
        assert!(count_x < shown.rect.x + shown.rect.width);
        let name_end = shown.rect.x
            + 3.0 * METRICS.padding
            + METRICS.dot()
            + 2.0 * METRICS.padding
            + measure(&shown.text, true);
        assert!(name_end <= count_x, "{name_end} {count_x}");
    }

    #[test]
    fn text_is_cut_and_wrapped_by_cells() {
        assert_eq!(cut("not found", 20), "not found");
        assert_eq!(cut("not found", 5), "not…");
        assert_eq!(cut("日本語のテキスト", 7), "日本語…");
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\nb", 10), ["a", "b"]);
    }
}
