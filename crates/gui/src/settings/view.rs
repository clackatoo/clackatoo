//! How the settings menu looks in Easy mode, and the header both modes
//! share: a panel over the panes, its header the title, one search, the
//! switch between Easy and Advanced and a close button; down its left
//! the pages, each with an icon; beside them the page picked, its rows in
//! cards under small headings, each a label, a line of help and a
//! control at the right; along its foot what was saved last, and keys.
//!
//! Its words are in the interface font, at several sizes, and values in
//! the terminal's font at the interface's size (see
//! [`TextFont::UiAt`] and [`TextFont::MonoAt`]); Appearance's sample is
//! in the terminal's own font and size, as terminals draw it. Sizes are
//! the design's, in points scaled by the window's padding.
//!
//! What's drawn and where clicks go are laid out together, once a
//! frame, into a [`Laid`], so a click goes where the frame showed it. Text
//! can't be cut off part way, so a row that doesn't fit isn't drawn, and
//! the page scrolls a row, a heading or the sample at a time.

use super::Mode;
use crate::chrome::{Geometry, Painted, push_shadow};
use crate::finder::view::Row;
use crate::layout::Rect;
use crate::render::icons::Icon;
use crate::render::{Fill, Ink, Paint, Text, TextFont, mono_width};

/// What Easy shows.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools, reason = "each is so or not")]
pub(crate) struct EasyView {
    /// What's typed to search; and whether it's being typed, with the
    /// caret after it.
    pub(crate) query: String,
    pub(crate) typing: bool,
    /// The page picked down the side, by its place there; none while a
    /// search shows what it found.
    pub(crate) page: Option<usize>,
    /// The keys go to the pages down the side, not to the rows.
    pub(crate) on_side: bool,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) preview: Option<Preview>,
    pub(crate) sections: Vec<SectionView>,
    /// What the page says at its foot, and whether it's a link to
    /// Advanced.
    pub(crate) note: Option<(&'static str, bool)>,
    /// The row the keys are on, counting the rows of every section.
    pub(crate) focus: Option<usize>,
    /// The first of the page's parts shown (see [`Laid::top`]), and
    /// whether the row focused is to be brought into view.
    pub(crate) top: usize,
    pub(crate) reveal: bool,
    /// What was saved last, or what went wrong, along the foot.
    pub(crate) footer: String,
    pub(crate) warning: bool,
    /// The font picker, open over the page.
    pub(crate) fonts: Option<FontsView>,
    /// The row whose slider is being dragged.
    pub(crate) sliding: Option<usize>,
    /// Whose the daemon's settings the page shows are, said in the
    /// header, if they're another machine's: `daemon on desk`.
    pub(crate) machine: Option<String>,
}

/// Appearance's sample: lines of text in runs of ink, as a terminal and
/// an editor show them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Preview {
    pub(crate) lines: Vec<Vec<(String, Ink)>>,
    /// The line lit as the cursor's, and where the cursor is: its line
    /// and cell.
    pub(crate) cursorline: Option<usize>,
    pub(crate) cursor: (usize, usize),
    /// How much bigger than the terminal's text it's drawn, while a size
    /// is being dragged to.
    pub(crate) scale: f32,
}

/// Rows under a heading.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SectionView {
    pub(crate) heading: String,
    pub(crate) rows: Vec<RowView>,
}

/// One row: what it is, and its control.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowView {
    pub(crate) label: String,
    pub(crate) help: String,
    /// Lines more, under the help, each in its ink: what it is now, if
    /// that's not what's saved.
    pub(crate) notes: Vec<(String, Ink)>,
    /// Where the user's own config sets it, which locks it: `init.lua:12`.
    pub(crate) lock: Option<String>,
    pub(crate) control: ControlView,
}

/// A row's control, as it's drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ControlView {
    /// A switch, on or off.
    Toggle(bool),
    /// Buttons side by side, the one picked lit, if one is.
    Segmented {
        labels: Vec<&'static str>,
        picked: Option<usize>,
    },
    /// A slider, `at` this far along, from 0 to 1, with its value.
    Slider { at: f32, value: String },
    /// A number between − and +.
    Stepper(String),
    /// A card for each agent.
    Cards(Vec<Card>),
    /// The font in use, as a button that opens the picker.
    Font(String),
    /// Text, in a box, being typed if `editing`.
    Field { text: String, editing: bool },
    /// The keys bound, each as it's written, and what's recorded so far,
    /// while keys are recorded.
    Keys {
        keys: Vec<String>,
        recording: Option<String>,
    },
    /// A setting only Advanced has, which a search found.
    Advanced,
}

/// An agent's card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Card {
    pub(crate) title: String,
    pub(crate) command: String,
    pub(crate) note: Option<(String, Ink)>,
    pub(crate) picked: bool,
}

/// The font picker, over the page, below its row's button.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FontsView {
    /// The row whose button it opened from.
    pub(crate) row: usize,
    pub(crate) query: String,
    pub(crate) count: String,
    /// Which fonts it lists: monospace ones, or every one.
    pub(crate) listing: &'static str,
    pub(crate) rows: Vec<Row>,
    pub(crate) selected: Option<usize>,
}

/// How many fonts the picker shows at once.
pub(crate) const FONT_ROWS: usize = 10;

/// What a point over the menu is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsHit {
    /// The panel, away from anything that acts.
    Panel,
    /// Off the panel, over what it dims: a click closes the menu.
    Backdrop,
    /// The search box.
    Search,
    /// The switch to a mode.
    Mode(Mode),
    Close,
    /// A page down the side, by its place there.
    Page(u8),
    /// A link to Advanced.
    Advanced,
    /// A row, away from its control, by its place among every section's.
    Row(u16),
    /// A part of a row's control.
    Control(u16, Part),
    /// The place a locked row is set in.
    Lock(u16),
    /// A font in the picker, by its place in it.
    Font(u16),
}

/// The part of a control a point is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    /// All of it: a switch, a button, a box.
    Whole,
    /// One of side-by-side buttons.
    Segment(u8),
    /// A slider, this many thousandths along.
    Slider(u16),
    /// The − of a number...
    Less,
    /// ...and its +.
    More,
    /// An agent's card.
    Card(u8),
}

/// What's needed to measure text beside the interface font's widths.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sizes {
    /// How wide a cell of the terminal's font is at the interface font's
    /// size (see [`crate::fonts::Fonts::mono_em`]).
    pub(crate) mono_em: f32,
}

/// Text laid out: where it goes, and how big it is.
#[derive(Clone, Debug)]
struct Placed {
    origin: [f32; 2],
    content: String,
    ink: Ink,
    font: TextFont,
    size: [f32; 2],
}

/// The menu laid out in a window: what to draw, and where clicks go.
#[derive(Clone, Debug, Default)]
pub(crate) struct Laid {
    panel: Rect,
    fills: Vec<Fill>,
    texts: Vec<Placed>,
    /// Where each thing that acts is, later ones over earlier ones.
    hits: Vec<(Rect, SettingsHit)>,
    /// What to draw over a thing while the mouse is over it.
    hovers: Vec<(SettingsHit, Fill)>,
    /// Each slider's track, by its row.
    sliders: Vec<(u16, Rect)>,
    /// The first part of the page shown, kept as the page scrolls: the
    /// sample, a heading, a row or the note at its foot, in order.
    pub(crate) top: usize,
    /// Whether it's all of Easy, rather than Advanced's header.
    modal: bool,
    /// The menu's whole panel, in either mode: off it, a click closes it.
    outer: Rect,
    /// The row whose slider is being dragged.
    sliding: Option<u16>,
}

impl Laid {
    /// Whether it's all of Easy, rather than Advanced's header.
    pub(crate) fn modal(&self) -> bool {
        self.modal
    }

    /// The menu's whole panel, in either mode.
    pub(crate) fn outer(&self) -> Rect {
        self.outer
    }

    /// What `point` is over, if it's over the panel; or for Easy, which
    /// dims the panes, anywhere.
    pub(crate) fn hit(&self, point: [f32; 2]) -> Option<SettingsHit> {
        if !self.panel.contains(point) {
            return None;
        }
        let hit = (self.hits.iter().rev())
            .find(|(rect, _)| rect.contains(point))
            .map_or(SettingsHit::Panel, |&(_, hit)| hit);
        Some(match hit {
            SettingsHit::Control(row, Part::Slider(_)) => {
                SettingsHit::Control(row, Part::Slider(self.slide(row, point[0]).unwrap_or(0)))
            }
            hit => hit,
        })
    }

    /// How far along the slider being dragged `x` is, in thousandths.
    pub(crate) fn sliding_at(&self, x: f32) -> Option<u16> {
        self.slide(self.sliding?, x)
    }

    /// How far along `row`'s slider `x` is, in thousandths, if it has one.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub(crate) fn slide(&self, row: u16, x: f32) -> Option<u16> {
        let (_, track) = self.sliders.iter().find(|(at, _)| *at == row)?;
        let along = ((x - track.x) / track.width.max(1.0)).clamp(0.0, 1.0);
        Some((along * 1000.0).round() as u16)
    }

    /// Adds what it draws to `painted`, over the panes, with what's under
    /// the mouse, `hover`, picked out.
    pub(crate) fn paint<'a>(&'a self, hover: Option<SettingsHit>, painted: &mut Painted<'a>) {
        painted.over.extend_from_slice(&self.fills);
        // A slider's hover goes by its row, wherever along it the mouse is.
        let hover = hover.map(|hit| match hit {
            SettingsHit::Control(row, Part::Slider(_)) => {
                SettingsHit::Control(row, Part::Slider(0))
            }
            hit => hit,
        });
        if let Some(hover) = hover {
            painted.over.extend(
                (self.hovers.iter())
                    .filter(|(hit, _)| *hit == hover)
                    .map(|(_, fill)| *fill),
            );
        }
        painted
            .over_texts
            .extend(self.texts.iter().map(|text| Text {
                origin: text.origin,
                content: &text.content,
                ink: text.ink,
                font: text.font,
            }));
    }
}

/// How tall the settings menu's header is, in either mode.
pub(crate) fn header_height(geometry: &Geometry) -> f32 {
    (64.0 * geometry.metrics.padding / 4.0).round()
}

/// Where the settings menu's panel goes in a window, in either mode: as
/// big as the design's, or as nearly as the window allows.
pub(crate) fn panel(geometry: &Geometry) -> Rect {
    let unit = geometry.metrics.padding / 4.0;
    let [width, height] = geometry.size;
    let margin = 4.0 * geometry.metrics.padding;
    let panel_width = (width * 0.875)
        .min(1180.0 * unit)
        .min(width - margin)
        .max(1.0)
        .round();
    let panel_height = (height * 0.875)
        .min(820.0 * unit)
        .min(height - margin)
        .max(1.0)
        .round();
    Rect {
        x: ((width - panel_width) / 2.0).round(),
        y: ((height - panel_height) / 2.0).round(),
        width: panel_width,
        height: panel_height,
    }
}

/// Lays out what's drawn, and where it is, from `measure`, which says how
/// wide text is in the interface font, bold or not.
struct Lay<'m> {
    geometry: Geometry,
    sizes: Sizes,
    /// A point of the design, in pixels.
    unit: f32,
    measure: &'m mut dyn FnMut(&str, bool) -> f32,
    laid: Laid,
}

/// Whether text in `ink` is drawn bold, as the renderer draws it.
fn bold(ink: Ink) -> bool {
    matches!(ink, Ink::Bold | Ink::Key | Ink::OnColor)
}

impl Lay<'_> {
    fn u(&self, points: f32) -> f32 {
        (points * self.unit).round()
    }

    /// How wide `text` is in `font`, in `ink`.
    #[allow(clippy::cast_precision_loss)]
    fn width(&mut self, text: &str, font: TextFont, ink: Ink) -> f32 {
        match font {
            TextFont::Mono => mono_width(text) as f32 * self.geometry.metrics.cell[0],
            TextFont::MonoAt(_) => mono_width(text) as f32 * self.sizes.mono_em * font.scale(),
            TextFont::Ui | TextFont::UiAt(_) => (self.measure)(text, bold(ink)) * font.scale(),
        }
    }

    /// How tall a line of `font` is.
    fn line(&self, font: TextFont) -> f32 {
        match font {
            TextFont::Mono => self.geometry.metrics.cell[1],
            _ => (self.geometry.metrics.ui_line * font.scale()).round(),
        }
    }

    /// Adds `text` with its top left at `origin`; returns how wide it is.
    fn text(&mut self, origin: [f32; 2], text: &str, ink: Ink, font: TextFont) -> f32 {
        let width = self.width(text, font, ink);
        let height = self.line(font);
        self.laid.texts.push(Placed {
            origin: [origin[0].round(), origin[1].round()],
            content: text.to_owned(),
            ink,
            font,
            size: [width, height],
        });
        width
    }

    /// Adds `text` centred on the line through `middle`, from `x`.
    fn text_mid(&mut self, x: f32, middle: f32, text: &str, ink: Ink, font: TextFont) -> f32 {
        let top = middle - self.line(font) / 2.0;
        self.text([x, top], text, ink, font)
    }

    fn fill(&mut self, fill: Fill) {
        self.laid.fills.push(fill);
    }

    fn hit(&mut self, rect: Rect, hit: SettingsHit) {
        self.laid.hits.push((rect, hit));
    }

    /// `fill`, drawn while the mouse is over `hit`.
    fn hover(&mut self, hit: SettingsHit, fill: Fill) {
        self.laid.hovers.push((hit, fill));
    }

    /// An icon `size` points square, centred on `middle`.
    fn icon(&mut self, middle: [f32; 2], size: f32, icon: Icon, paint: Paint) {
        let square = self.u(size);
        self.fill(Fill::icon(
            [
                (middle[0] - square / 2.0).round(),
                (middle[1] - square / 2.0).round(),
                square,
                square,
            ],
            icon,
            paint,
        ));
    }

    /// `runs` of words, broken onto lines no wider than `width`, from
    /// `origin`, in `font`. Returns how tall they are, and where each run
    /// marked as a link went.
    fn paragraph(
        &mut self,
        origin: [f32; 2],
        width: f32,
        runs: &[(&str, Ink, bool)],
        font: TextFont,
    ) -> (f32, Vec<Rect>) {
        let height = self.line(font);
        let space = self.width(" ", font, Ink::Plain);
        let [mut x, mut y] = origin;
        let mut links = Vec::new();
        let mut first = true;
        for &(run, ink, link) in runs {
            for word in run.split(' ').filter(|word| !word.is_empty()) {
                let wide = self.width(word, font, ink);
                if !first && x + space + wide > origin[0] + width {
                    x = origin[0];
                    y += height;
                } else if !first {
                    x += space;
                }
                first = false;
                self.text([x, y], word, ink, font);
                if link {
                    links.push(Rect {
                        x,
                        y,
                        width: wide,
                        height,
                    });
                }
                x += wide;
            }
        }
        (y + height - origin[1], links)
    }

    /// How many lines `text` takes broken onto lines no wider than
    /// `width`, in `font`.
    fn lines_of(&mut self, text: &str, width: f32, font: TextFont, ink: Ink) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut line = String::new();
        for word in text.split(' ').filter(|word| !word.is_empty()) {
            let tried = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && self.width(&tried, font, ink) > width {
                lines.push(std::mem::replace(&mut line, word.to_owned()));
            } else {
                line = tried;
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
        lines
    }
}

/// The interface font's sizes, as the design's, for text 14 points high.
const TITLE: TextFont = TextFont::UiAt(157);
const HEADER: TextFont = TextFont::UiAt(121);
const HELP: TextFont = TextFont::UiAt(93);
const SMALL: TextFont = TextFont::UiAt(86);
const CARD_TITLE: TextFont = TextFont::UiAt(107);
const VALUE: TextFont = TextFont::MonoAt(93);
const SMALL_VALUE: TextFont = TextFont::MonoAt(86);

/// The header's switch between the modes and its close button, laid out
/// at the right of `line`, with `mode` lit. Returns where they start.
fn lay_switch(lay: &mut Lay<'_>, line: Rect, mode: Mode) -> f32 {
    let middle = line.y + line.height / 2.0;
    // Close, at the far right.
    let side = lay.u(36.0);
    let close = Rect {
        x: line.right() - lay.u(20.0) - side,
        y: (middle - side / 2.0).round(),
        width: side,
        height: side,
    };
    lay.hover(
        SettingsHit::Close,
        Fill::rounded(close.to_array(), Paint::ChatHover, lay.u(8.0)),
    );
    lay.icon(
        [close.x + side / 2.0, middle],
        18.0,
        Icon::Close,
        Paint::Icon,
    );
    lay.hit(close, SettingsHit::Close);
    // Easy and Advanced, side by side in a well.
    let labels = [(Mode::Easy, "Easy"), (Mode::Advanced, "Advanced")];
    let pad = lay.u(14.0);
    let widths: Vec<f32> = (labels.iter())
        .map(|&(each, label)| {
            let ink = if each == mode {
                Ink::OnColor
            } else {
                Ink::Muted
            };
            lay.width(label, HELP, ink) + 2.0 * pad
        })
        .collect();
    let inset = lay.u(3.0);
    let well = Rect {
        width: widths.iter().sum::<f32>() + 2.0 * inset,
        height: lay.u(30.0) + 2.0 * inset,
        x: 0.0,
        y: 0.0,
    };
    let well = Rect {
        x: close.x - lay.u(16.0) - well.width,
        y: (middle - well.height / 2.0).round(),
        ..well
    };
    lay.fill(
        Fill::rounded(well.to_array(), Paint::Well, lay.u(9.0))
            .bordered(lay.geometry.edge, Paint::PaletteEdge),
    );
    let mut x = well.x + inset;
    for (&(each, label), width) in labels.iter().zip(widths) {
        let button = Rect {
            x,
            y: well.y + inset,
            width,
            height: well.height - 2.0 * inset,
        };
        let hit = SettingsHit::Mode(each);
        if each == mode {
            lay.fill(Fill::rounded(button.to_array(), Paint::Accent, lay.u(6.0)));
            lay.text_mid(x + pad, middle, label, Ink::OnColor, HELP);
        } else {
            let rounded = Fill::rounded(button.to_array(), Paint::ChatHover, lay.u(6.0));
            lay.hover(hit, rounded);
            lay.text_mid(x + pad, middle, label, Ink::Muted, HELP);
        }
        lay.hit(button, hit);
        x += width;
    }
    well.x
}

/// What the header's search box says: what's typed, as it's shown, in the
/// interface's font, or, as Advanced's is, the terminal's; whether it's
/// being typed; and at its right, how many match.
pub(crate) struct Search<'a> {
    pub(crate) shown: &'a str,
    pub(crate) mono: bool,
    pub(crate) typing: bool,
    pub(crate) count: Option<&'a str>,
}

/// Lays out the header both modes share along `header`: the title, the
/// search box, the switch with `mode` lit, and the close button.
fn lay_header(lay: &mut Lay<'_>, header: Rect, mode: Mode, search: &Search<'_>) {
    let edge = lay.geometry.edge;
    let middle = header.y + header.height / 2.0;
    let title_x = header.x + lay.u(24.0);
    let title_width = lay.text_mid(title_x, middle, "Settings", Ink::Bold, HEADER);
    let switch_x = lay_switch(lay, header, mode);
    let rect = Rect {
        x: title_x + title_width + lay.u(28.0),
        y: (middle - lay.u(19.0)).round(),
        width: 0.0,
        height: lay.u(38.0),
    };
    let rect = Rect {
        width: (switch_x - lay.u(16.0) - rect.x).max(0.0),
        ..rect
    };
    let border = if search.typing {
        Paint::Accent
    } else {
        Paint::PaletteEdge
    };
    lay.fill(Fill::rounded(rect.to_array(), Paint::Well, lay.u(9.0)).bordered(edge, border));
    lay.icon(
        [rect.x + lay.u(22.0), middle],
        16.0,
        Icon::Search,
        Paint::Icon,
    );
    let text_x = rect.x + lay.u(40.0);
    let mut right = rect.right() - lay.u(14.0);
    if let Some(count) = search.count {
        right -= lay.width(count, SMALL_VALUE, Ink::Muted);
        lay.text_mid(right, middle, count, Ink::Muted, SMALL_VALUE);
    }
    if search.shown.is_empty() {
        let placeholder = if search.mono {
            "numberwidth, scrolloff, keys…"
        } else {
            "Search every setting…"
        };
        lay.text_mid(text_x, middle, placeholder, Ink::Muted, TextFont::Ui);
        if search.count.is_none() {
            // The key that starts a search.
            let cap = Rect {
                x: right - lay.u(20.0),
                y: (middle - lay.u(10.0)).round(),
                width: lay.u(20.0),
                height: lay.u(20.0),
            };
            lay.fill(
                Fill::rounded(cap.to_array(), Paint::Clear, lay.u(5.0))
                    .bordered(edge, Paint::PaletteEdge),
            );
            let slash = lay.width("/", SMALL_VALUE, Ink::Muted);
            lay.text_mid(
                cap.x + (cap.width - slash) / 2.0,
                middle,
                "/",
                Ink::Muted,
                SMALL_VALUE,
            );
        }
    } else {
        let font = if search.mono {
            TextFont::Mono
        } else {
            TextFont::Ui
        };
        lay.text_mid(text_x, middle, search.shown, Ink::Plain, font);
    }
    lay.hit(rect, SettingsHit::Search);
    lay.fill(Fill::solid(
        [header.x, header.bottom(), header.width, edge],
        Paint::PaletteEdge,
    ));
}

/// Lays out Advanced's header along `header`, the top of the finder's
/// panel, where the finder leaves room for it (see
/// [`crate::finder::view::Layout`]).
pub(crate) fn lay_out_header(
    header: Rect,
    search: &Search<'_>,
    geometry: &Geometry,
    sizes: Sizes,
    measure: &mut dyn FnMut(&str, bool) -> f32,
) -> Laid {
    let mut lay = Lay {
        geometry: *geometry,
        sizes,
        unit: geometry.metrics.padding / 4.0,
        measure,
        laid: Laid {
            panel: header,
            outer: panel(geometry),
            ..Laid::default()
        },
    };
    lay_header(&mut lay, header, Mode::Advanced, search);
    lay.laid
}

/// Lays out `view` in a window of `geometry`.
#[allow(clippy::too_many_lines, reason = "the panel's parts, top to bottom")]
pub(crate) fn lay_out(
    view: &EasyView,
    geometry: &Geometry,
    sizes: Sizes,
    measure: &mut dyn FnMut(&str, bool) -> f32,
) -> Laid {
    let panel = panel(geometry);
    let mut lay = Lay {
        geometry: *geometry,
        sizes,
        unit: geometry.metrics.padding / 4.0,
        measure,
        laid: Laid {
            panel,
            outer: panel,
            modal: true,
            sliding: view.sliding.and_then(|row| u16::try_from(row).ok()),
            ..Laid::default()
        },
    };
    let edge = geometry.edge;
    let [window_width, window_height] = geometry.size;
    // The panes behind, dimmed, and the panel raised over them.
    lay.fill(Fill::solid(
        [0.0, 0.0, window_width, window_height],
        Paint::Dim,
    ));
    let radius = lay.u(14.0);
    push_shadow(&mut lay.laid.fills, panel, radius, geometry.metrics.padding);
    lay.fill(
        Fill::rounded(panel.to_array(), Paint::Palette, lay.u(14.0))
            .bordered(edge, Paint::PaletteEdge),
    );

    // The header: the title, the search, the switch and close.
    let header = Rect {
        height: header_height(geometry),
        ..panel
    };
    let shown = if view.typing {
        crate::picker::drawn(&view.query)
    } else {
        view.query.clone()
    };
    let search = Search {
        shown: &shown,
        mono: false,
        typing: view.typing,
        count: view.machine.as_deref(),
    };
    lay_header(&mut lay, header, Mode::Easy, &search);

    // The foot: what was saved, then keys.
    let foot = Rect {
        y: panel.bottom() - lay.u(44.0),
        height: lay.u(44.0),
        ..panel
    };
    lay.fill(Fill::solid(
        [panel.x, foot.y, panel.width, edge],
        Paint::PaletteEdge,
    ));
    let foot_middle = foot.y + foot.height / 2.0;
    let mut x = foot.x + lay.u(24.0);
    if !view.warning {
        lay.icon(
            [x + lay.u(7.0), foot_middle],
            14.0,
            Icon::Check,
            Paint::Agents,
        );
        x += lay.u(24.0);
    }
    let said_ink = if view.warning {
        Ink::Warning
    } else {
        Ink::Muted
    };
    lay.text_mid(x, foot_middle, &view.footer, said_ink, HELP);
    let mut right = foot.right() - lay.u(24.0);
    let hints: &[(&str, &str)] = if view.fonts.is_some() {
        &[("Enter", "pick"), ("Tab", "every font"), ("Esc", "back")]
    } else {
        &[
            ("u", "undo"),
            ("r", "default"),
            ("m", "Advanced"),
            ("Esc", "close"),
        ]
    };
    for (index, &(key, does)) in hints.iter().enumerate().rev() {
        let does = format!(" {does}");
        right -= lay.width(&does, SMALL, Ink::Muted);
        lay.text_mid(right, foot_middle, &does, Ink::Muted, SMALL);
        right -= lay.width(key, SMALL, Ink::Key);
        lay.text_mid(right, foot_middle, key, Ink::Key, SMALL);
        if index > 0 {
            right -= lay.width(" · ", SMALL, Ink::Muted);
            lay.text_mid(right, foot_middle, " · ", Ink::Muted, SMALL);
        }
    }

    // The pages, down the side.
    let body = Rect {
        y: header.bottom() + edge,
        height: (foot.y - header.bottom() - edge).max(0.0),
        ..panel
    };
    let side = Rect {
        width: lay.u(248.0).min(panel.width / 3.0),
        ..body
    };
    lay.fill(Fill::solid(
        [side.right(), body.y, edge, body.height],
        Paint::PaletteEdge,
    ));
    let mut y = side.y + lay.u(16.0);
    let item_height = lay.u(40.0);
    for (index, page) in crate::options::easy::Page::ALL.iter().enumerate() {
        let item = Rect {
            x: side.x + lay.u(12.0),
            y,
            width: side.width - lay.u(24.0),
            height: item_height,
        };
        if item.bottom() > body.bottom() {
            break;
        }
        let hit = SettingsHit::Page(u8::try_from(index).unwrap_or(u8::MAX));
        let picked = view.page == Some(index);
        let radius = lay.u(8.0);
        if picked {
            lay.fill(Fill::rounded(item.to_array(), Paint::Chosen, radius));
            if view.on_side {
                lay.fill(
                    Fill::rounded(item.to_array(), Paint::Clear, radius)
                        .bordered(lay.u(1.5), Paint::Accent),
                );
            }
        } else {
            lay.hover(
                hit,
                Fill::rounded(item.to_array(), Paint::ChatHover, radius),
            );
        }
        let middle = item.y + item.height / 2.0;
        let icon_paint = if picked { Paint::Accent } else { Paint::Icon };
        lay.icon(
            [item.x + lay.u(21.0), middle],
            18.0,
            page.icon(),
            icon_paint,
        );
        let ink = if picked { Ink::Bold } else { Ink::Plain };
        lay.text_mid(
            item.x + lay.u(42.0),
            middle,
            page.title(),
            ink,
            TextFont::Ui,
        );
        lay.hit(item, hit);
        y += item_height + lay.u(2.0);
    }
    // At the side's foot, where everything else is.
    let note_width = side.width - lay.u(48.0);
    let runs = [
        (
            "Every setting, with its file and line, is in",
            Ink::Muted,
            false,
        ),
        ("Advanced.", Ink::Accent, true),
    ];
    let note_lines = {
        let text = "Every setting, with its file and line, is in Advanced.";
        lay.lines_of(text, note_width, SMALL, Ink::Muted).len()
    };
    #[allow(clippy::cast_precision_loss)]
    let note_height = note_lines as f32 * lay.line(SMALL);
    let note_y = body.bottom() - lay.u(20.0) - note_height;
    if note_y > y {
        let (_, links) = lay.paragraph([side.x + lay.u(24.0), note_y], note_width, &runs, SMALL);
        for link in links {
            lay.hit(link, SettingsHit::Advanced);
        }
    }

    // The page.
    let main_x = side.right() + lay.u(40.0);
    let main = Rect {
        x: main_x,
        y: body.y + lay.u(28.0),
        width: (panel.right() - lay.u(40.0) - main_x)
            .min(lay.u(860.0))
            .max(0.0),
        height: 0.0,
    };
    let title_height = lay.line(TITLE);
    lay.text([main.x, main.y], &view.title, Ink::Bold, TITLE);
    let subtitle_y = main.y + title_height + lay.u(6.0);
    lay.text(
        [main.x, subtitle_y],
        &view.subtitle,
        Ink::Muted,
        TextFont::Ui,
    );
    let room = Rect {
        y: subtitle_y + lay.line(TextFont::Ui) + lay.u(22.0),
        height: 0.0,
        ..main
    };
    let room = Rect {
        height: (foot.y - lay.u(12.0) - room.y).max(0.0),
        ..room
    };
    lay_page(&mut lay, view, room);
    lay.laid
}

/// One of the parts a page is made of, top to bottom, which it scrolls by.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Block {
    Preview,
    /// A section's heading, by the section's place.
    Heading(usize),
    /// A row, by its section's place and its place among every row.
    Row(usize, usize),
    Note,
}

/// Lays out the page's sample, sections, rows and note in `room`, from
/// the part `view.top`, as far down as they fit.
#[allow(clippy::too_many_lines, reason = "each part of a page in turn")]
fn lay_page(lay: &mut Lay<'_>, view: &EasyView, room: Rect) {
    // What's to be drawn, and how tall each is.
    let mut blocks: Vec<Block> = Vec::new();
    if view.preview.is_some() {
        blocks.push(Block::Preview);
    }
    let mut flat = 0;
    for (section, each) in view.sections.iter().enumerate() {
        blocks.push(Block::Heading(section));
        for _ in &each.rows {
            blocks.push(Block::Row(section, flat));
            flat += 1;
        }
    }
    if view.note.is_some() {
        blocks.push(Block::Note);
    }
    let rows: Vec<&RowView> = view.sections.iter().flat_map(|each| &each.rows).collect();
    // Each row's words broken to fit, once.
    let shaped: Vec<Shaped> = (rows.iter())
        .map(|row| shape(lay, row, room.width))
        .collect();
    let heights: Vec<f32> = (blocks.iter())
        .map(|&block| block_height(lay, view, &shaped, block, room.width))
        .collect();
    // The gap before each, after the one before it.
    let gaps: Vec<f32> = (blocks.iter().enumerate())
        .map(
            |(at, block)| match (at.checked_sub(1).map(|at| blocks[at]), block) {
                (None, _) | (Some(Block::Row(..)), Block::Row(..)) => 0.0,
                (Some(Block::Heading(_)), Block::Row(..)) => lay.u(10.0),
                _ => lay.u(22.0),
            },
        )
        .collect();
    // The last part shown from `top`, and whether all from it fit.
    let last_shown = |top: usize| -> usize {
        let mut used = 0.0;
        let mut last = top;
        // The part at the top, and a heading's first row, are drawn even if
        // taller than all the room, cut off at its foot.
        let must = match blocks.get(top) {
            Some(Block::Heading(_)) => top + 1,
            _ => top,
        };
        for at in top..blocks.len() {
            let gap = if at == top { 0.0 } else { gaps[at] };
            if used + gap + heights[at] > room.height && at > must {
                break;
            }
            used += gap + heights[at];
            last = at;
        }
        last
    };
    // A heading isn't drawn without a row under it.
    let last_shown = |top: usize| -> usize {
        let last = last_shown(top);
        if last > top && last + 1 < blocks.len() && matches!(blocks[last], Block::Heading(_)) {
            last - 1
        } else {
            last
        }
    };
    let most = (0..blocks.len())
        .find(|&top| last_shown(top) + 1 >= blocks.len())
        .unwrap_or(0);
    let mut top = view.top.min(most);
    if view.reveal
        && let Some(focus) = view.focus
        && let Some(at) = blocks
            .iter()
            .position(|block| matches!(block, Block::Row(_, row) if *row == focus))
    {
        if at < top {
            // With its heading, if it's the first under it.
            top = if at > 0 && matches!(blocks[at - 1], Block::Heading(_)) {
                at - 1
            } else {
                at
            };
        }
        while top < at && last_shown(top) < at {
            top += 1;
        }
    }
    lay.laid.top = top;

    // Draw from `top`, each section's rows in a card.
    let mut y = room.y;
    let last = last_shown(top);
    let mut card: Option<(f32, usize)> = None;
    let card_paint = |lay: &mut Lay<'_>, from: f32, to: f32| {
        lay.laid.fills.push(
            Fill::rounded(
                [room.x, from, room.width, to - from],
                Paint::Card,
                lay.u(12.0),
            )
            .bordered(lay.geometry.edge, Paint::CardEdge),
        );
    };
    // Cards are drawn under their rows: their fills go in first, so the
    // rows' are laid out into a list of their own and added after.
    let mut row_fills: Vec<Fill> = Vec::new();
    let mut card_fills: Vec<Fill> = Vec::new();
    // What the page draws, to cut off at its foot: a part taller than all
    // the room there is is drawn as far as the room goes.
    let (fills_from, texts_from, hits_from, hovers_from) = (
        lay.laid.fills.len(),
        lay.laid.texts.len(),
        lay.laid.hits.len(),
        lay.laid.hovers.len(),
    );
    let shown = if blocks.is_empty() {
        0..0
    } else {
        top..last + 1
    };
    for at in shown {
        if at > top {
            y += gaps[at];
        }
        let height = heights[at];
        let block = blocks[at];
        // A card ends where its section's rows do.
        if let Some((from, section)) = card
            && !matches!(block, Block::Row(each, _) if each == section)
        {
            card_paint(lay, from, y - gaps[at]);
            card_fills.extend(lay.laid.fills.pop());
            card = None;
        }
        match block {
            Block::Preview => {
                if let Some(preview) = &view.preview {
                    lay_preview(lay, preview, Rect { y, height, ..room });
                }
            }
            Block::Heading(section) => {
                let heading = view.sections[section].heading.to_uppercase();
                lay.text([room.x, y], &heading, Ink::Muted, SMALL);
            }
            Block::Row(section, row) => {
                let cards = matches!(rows[row].control, ControlView::Cards(_));
                let first_in_card = card.is_none() && !cards;
                if first_in_card {
                    card = Some((y, section));
                }
                let before = lay.laid.fills.len();
                lay_row(
                    lay,
                    view,
                    (rows[row], &shaped[row]),
                    row,
                    Rect { y, height, ..room },
                    !first_in_card && !cards,
                );
                row_fills.extend(lay.laid.fills.drain(before..));
            }
            Block::Note => {
                if let Some((note, link)) = view.note {
                    let runs: Vec<(&str, Ink, bool)> = if link {
                        let (start, _) =
                            note.split_at(note.rfind("Advanced").unwrap_or(note.len()));
                        vec![
                            (start, Ink::Muted, false),
                            (&note[start.len()..], Ink::Accent, true),
                        ]
                    } else {
                        vec![(note, Ink::Muted, false)]
                    };
                    let (_, links) = lay.paragraph([room.x, y], room.width, &runs, HELP);
                    for link in links {
                        lay.hit(link, SettingsHit::Advanced);
                    }
                }
            }
        }
        y += height;
    }
    if let Some((from, _)) = card {
        card_paint(lay, from, y);
        card_fills.extend(lay.laid.fills.pop());
    }
    lay.laid.fills.extend(card_fills);
    lay.laid.fills.extend(row_fills);
    let bottom = room.bottom();
    let clip = [
        room.x - lay.u(8.0),
        room.y - lay.u(8.0),
        room.width + lay.u(16.0),
        bottom - room.y + lay.u(8.0),
    ];
    for fill in &mut lay.laid.fills[fills_from..] {
        *fill = fill.clipped(clip);
    }
    for (_, fill) in &mut lay.laid.hovers[hovers_from..] {
        *fill = fill.clipped(clip);
    }
    let texts = lay.laid.texts.split_off(texts_from);
    (lay.laid.texts).extend(
        texts
            .into_iter()
            .filter(|text| text.origin[1] + text.size[1] <= bottom),
    );
    let hits = lay.laid.hits.split_off(hits_from);
    (lay.laid.hits).extend(hits.into_iter().filter_map(|(rect, hit)| {
        let height = rect.height.min(bottom - rect.y);
        (height > 0.0).then_some((Rect { height, ..rect }, hit))
    }));
    // A bar down the page's right, if it's longer than the room, saying
    // how much is above and below.
    if !blocks.is_empty() && (top > 0 || last + 1 < blocks.len()) {
        let total: f32 = heights
            .iter()
            .zip(&gaps)
            .map(|(height, gap)| height + gap)
            .sum();
        let before: f32 = (heights[..top].iter().zip(&gaps[..top]))
            .map(|(height, gap)| height + gap)
            .sum();
        let shown = y - room.y;
        let track = Rect {
            x: room.right() + lay.u(14.0),
            width: lay.u(4.0),
            ..room
        };
        let thumb_height = (track.height * shown / total.max(1.0)).max(lay.u(24.0));
        let thumb_y =
            track.y + (track.height - thumb_height) * (before / (total - shown).max(1.0)).min(1.0);
        lay.fill(Fill::rounded(
            track.to_array(),
            Paint::CardEdge,
            track.width / 2.0,
        ));
        lay.fill(Fill::rounded(
            [track.x, thumb_y.round(), track.width, thumb_height.round()],
            Paint::Track,
            track.width / 2.0,
        ));
    }
    if let Some(fonts) = &view.fonts {
        lay_fonts(lay, fonts, room);
    }
}

/// How tall `block` is, in a page `width` wide, its rows `shaped`.
#[allow(clippy::cast_precision_loss)]
fn block_height(
    lay: &mut Lay<'_>,
    view: &EasyView,
    shaped: &[Shaped],
    block: Block,
    width: f32,
) -> f32 {
    match block {
        Block::Preview => {
            let preview = view.preview.as_ref();
            let lines = preview.map_or(0, |preview| preview.lines.len());
            let scale = preview.map_or(1.0, |preview| preview.scale);
            lay.u(32.0) + lines as f32 * preview_line(lay, scale)
        }
        Block::Heading(_) => lay.line(SMALL),
        Block::Note => {
            let note = view.note.map_or("", |(note, _)| note);
            let lines = lay.lines_of(note, width, HELP, Ink::Muted).len();
            lines as f32 * lay.line(HELP)
        }
        Block::Row(_, row) => shaped[row].height,
    }
}

/// How tall a line of the sample is, drawn `scale` times as big as the
/// terminal's text.
fn preview_line(lay: &Lay<'_>, scale: f32) -> f32 {
    (lay.geometry.metrics.cell[1] * scale * 1.2).round()
}

/// A row as it fits a page: its words broken into lines, where its control
/// and the place it's locked at go, and how tall it all is, worked out
/// once a frame.
#[derive(Debug)]
struct Shaped {
    label: Vec<String>,
    /// Its help and notes, a line each, in their inks.
    lines: Vec<(String, Ink)>,
    control: [f32; 2],
    /// How wide the chip saying where it's locked is, with the gap after
    /// it, or 0.
    lock: f32,
    /// The control goes under the words, as there's no room beside them.
    stacked: bool,
    /// How tall the words are; for agents' cards, their grid.
    words: f32,
    height: f32,
}

/// The narrowest the words beside a control may be, in points: any
/// narrower, and the control goes under them.
const NARROWEST_WORDS: f32 = 160.0;

/// `row` as it fits a page `width` wide.
#[allow(clippy::cast_precision_loss)]
fn shape(lay: &mut Lay<'_>, row: &RowView, width: f32) -> Shaped {
    let lock = lock_width(lay, row);
    if let ControlView::Cards(cards) = &row.control {
        let columns = cards.len().clamp(1, 4);
        let rows = cards.len().div_ceil(columns);
        let grid = rows as f32 * lay.u(96.0) + rows.saturating_sub(1) as f32 * lay.u(12.0);
        // Under the cards, where they're locked, and what they are now;
        // the help only says why they're locked.
        let help = if row.lock.is_some() {
            &row.help[..]
        } else {
            ""
        };
        let lines = help_lines(lay, help, &row.notes, (width - lock).max(1.0));
        let words = lines.len() as f32 * lay.line(HELP);
        let chip = if lock > 0.0 { lay.u(28.0) } else { 0.0 };
        let foot = if words > 0.0 || chip > 0.0 {
            lay.u(10.0) + words.max(chip)
        } else {
            0.0
        };
        return Shaped {
            label: Vec::new(),
            lines,
            control: [0.0, 0.0],
            lock,
            stacked: false,
            words: grid,
            height: grid + foot,
        };
    }
    let control = control_size(lay, &row.control);
    let beside = width - lay.u(36.0) - control[0] - lay.u(24.0) - lock;
    let stacked = beside < lay.u(NARROWEST_WORDS);
    let text_width = if stacked { width - lay.u(36.0) } else { beside };
    let label = lay.lines_of(&row.label, text_width, TextFont::Ui, Ink::Plain);
    let lines = help_lines(lay, &row.help, &row.notes, text_width);
    let mut words = label.len().max(1) as f32 * lay.line(TextFont::Ui);
    if !lines.is_empty() {
        words += lay.u(2.0) + lines.len() as f32 * lay.line(HELP);
    }
    let chip = if lock > 0.0 { lay.u(28.0) } else { 0.0 };
    let height = if stacked {
        words + lay.u(10.0) + control[1].max(chip) + lay.u(28.0)
    } else {
        words.max(control[1]).max(chip) + lay.u(28.0)
    };
    Shaped {
        label,
        lines,
        control,
        lock,
        stacked,
        words,
        height,
    }
}

/// `help`, then `notes`, broken onto lines `width` wide, each in its
/// ink.
fn help_lines(
    lay: &mut Lay<'_>,
    help: &str,
    notes: &[(String, Ink)],
    width: f32,
) -> Vec<(String, Ink)> {
    let mut lines = Vec::new();
    for line in lay.lines_of(help, width, HELP, Ink::Muted) {
        lines.push((line, Ink::Muted));
    }
    for (note, ink) in notes {
        for line in lay.lines_of(note, width, HELP, *ink) {
            lines.push((line, *ink));
        }
    }
    lines
}

/// How wide the chip saying where a locked row is set is, with the gap
/// after it, or 0.
fn lock_width(lay: &mut Lay<'_>, row: &RowView) -> f32 {
    row.lock.as_ref().map_or(0.0, |lock| {
        lay.width(lock, SMALL_VALUE, Ink::Accent) + lay.u(38.0) + lay.u(12.0)
    })
}

/// How big a control is drawn: its width and height.
#[allow(clippy::cast_precision_loss)]
fn control_size(lay: &mut Lay<'_>, control: &ControlView) -> [f32; 2] {
    match control {
        ControlView::Toggle(_) => [lay.u(44.0), lay.u(26.0)],
        ControlView::Segmented { labels, .. } => {
            let mut width = lay.u(6.0);
            for label in labels {
                width += lay.width(label, HELP, Ink::OnColor) + lay.u(28.0);
            }
            [width, lay.u(34.0)]
        }
        ControlView::Slider { .. } => [lay.u(260.0), lay.u(20.0)],
        ControlView::Stepper(_) => [lay.u(110.0), lay.u(34.0)],
        ControlView::Font(name) => {
            let text = lay.width(name, VALUE, Ink::Plain);
            [(text + lay.u(50.0)).max(lay.u(220.0)), lay.u(36.0)]
        }
        ControlView::Field { text, .. } => {
            let text = lay.width(text, VALUE, Ink::Plain);
            [(text + lay.u(28.0)).max(lay.u(180.0)), lay.u(34.0)]
        }
        ControlView::Keys { keys, recording } => {
            let mut width = 0.0;
            match recording {
                Some(recording) => {
                    width += lay.width(recording, SMALL_VALUE, Ink::Accent) + lay.u(20.0);
                }
                None => {
                    for keys in keys.iter().take(3) {
                        width += lay.width(keys, SMALL_VALUE, Ink::Plain) + lay.u(28.0);
                    }
                }
            }
            width += lay.width("Record…", HELP, Ink::Plain) + lay.u(28.0);
            [width, lay.u(30.0)]
        }
        ControlView::Advanced => [
            lay.width("in Advanced", HELP, Ink::Accent) + lay.u(28.0),
            lay.u(28.0),
        ],
        ControlView::Cards(_) => [0.0, 0.0],
    }
}

/// Lays out the chip saying `place` is where row `number` is locked, from
/// `x`, centred on `middle`.
fn lay_lock(lay: &mut Lay<'_>, place: &str, number: u16, x: f32, middle: f32) {
    let text = lay.width(place, SMALL_VALUE, Ink::Accent);
    let chip = Rect {
        x,
        y: (middle - lay.u(14.0)).round(),
        width: text + lay.u(38.0),
        height: lay.u(28.0),
    };
    lay.fill(
        Fill::rounded(chip.to_array(), Paint::AccentFaint, lay.u(14.0))
            .bordered(lay.geometry.edge, Paint::AccentEdge),
    );
    lay.icon(
        [chip.x + lay.u(17.0), middle],
        12.0,
        Icon::Lock,
        Paint::Accent,
    );
    lay.text_mid(
        chip.x + lay.u(28.0),
        middle,
        place,
        Ink::Accent,
        SMALL_VALUE,
    );
    let hit = SettingsHit::Lock(number);
    lay.hover(
        hit,
        Fill::rounded(chip.to_array(), Paint::AccentFaint, lay.u(14.0)),
    );
    lay.hit(chip, hit);
}

/// Lays out `row`, as `shaped`, numbered `index` among every section's
/// rows, in `rect`, with a line above it if `divided`.
#[allow(
    clippy::too_many_lines,
    reason = "a row's text, its lock and its control"
)]
fn lay_row(
    lay: &mut Lay<'_>,
    view: &EasyView,
    (row, shaped): (&RowView, &Shaped),
    index: usize,
    rect: Rect,
    divided: bool,
) {
    let number = u16::try_from(index).unwrap_or(u16::MAX);
    let focused = view.focus == Some(index) && !view.on_side;
    let locked = row.lock.is_some();
    if let ControlView::Cards(cards) = &row.control {
        let grid = Rect {
            height: shaped.words,
            ..rect
        };
        lay_cards(lay, cards, number, grid, focused, locked);
        // Under them, where they're locked, and what they are now.
        let y = grid.bottom() + lay.u(10.0);
        if let Some(place) = &row.lock {
            lay_lock(lay, place, number, rect.x, y + lay.u(14.0));
        }
        let mut y = y;
        for (line, ink) in &shaped.lines {
            lay.text([rect.x + shaped.lock, y], line, *ink, HELP);
            y += lay.line(HELP);
        }
        return;
    }
    if divided {
        lay.fill(Fill::solid(
            [rect.x, rect.y, rect.width, lay.geometry.edge],
            Paint::CardEdge,
        ));
    }
    if focused {
        lay.fill(Fill::solid(
            [
                rect.x + lay.geometry.edge,
                rect.y + lay.geometry.edge,
                rect.width - 2.0 * lay.geometry.edge,
                rect.height - lay.geometry.edge,
            ],
            Paint::ChatHover,
        ));
    }
    lay.hit(rect, SettingsHit::Row(number));
    let size = shaped.control;
    let x = rect.x + lay.u(18.0);
    // The words, at the left; the control right of them, or under them.
    let (mut y, control) = if shaped.stacked {
        let top = rect.y + lay.u(14.0);
        let middle = top + shaped.words + lay.u(10.0) + size[1].max(lay.u(28.0)) / 2.0;
        let control = Rect {
            x: x + shaped.lock,
            y: (middle - size[1] / 2.0).round(),
            width: size[0],
            height: size[1],
        };
        (top, control)
    } else {
        let middle = rect.y + rect.height / 2.0;
        let control = Rect {
            x: rect.right() - lay.u(18.0) - size[0],
            y: (middle - size[1] / 2.0).round(),
            width: size[0],
            height: size[1],
        };
        ((middle - shaped.words / 2.0).round(), control)
    };
    let label_ink = if locked { Ink::Muted } else { Ink::Plain };
    for line in &shaped.label {
        lay.text([x, y], line, label_ink, TextFont::Ui);
        y += lay.line(TextFont::Ui);
    }
    y += lay.u(2.0);
    for (line, ink) in &shaped.lines {
        lay.text([x, y], line, *ink, HELP);
        y += lay.line(HELP);
    }
    // Where it's locked, before the control.
    if let Some(place) = &row.lock {
        let middle = control.y + control.height / 2.0;
        let from = if shaped.stacked {
            x
        } else {
            control.x - shaped.lock
        };
        lay_lock(lay, place, number, from, middle);
    }
    lay_control(lay, &row.control, control, number, locked);
    if focused && !locked {
        let ring = lay.u(3.0);
        lay.fill(
            Fill::rounded(
                [
                    control.x - ring,
                    control.y - ring,
                    control.width + 2.0 * ring,
                    control.height + 2.0 * ring,
                ],
                Paint::Clear,
                lay.u(10.0),
            )
            .bordered(lay.u(1.5), Paint::Accent),
        );
    }
}

/// Lays out a row's `control` in `rect`, greyed if `locked`.
#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    reason = "each kind of control"
)]
fn lay_control(lay: &mut Lay<'_>, control: &ControlView, rect: Rect, row: u16, locked: bool) {
    let middle = rect.y + rect.height / 2.0;
    let edge = lay.geometry.edge;
    let whole = SettingsHit::Control(row, Part::Whole);
    match control {
        ControlView::Toggle(on) => {
            let track = if *on && !locked {
                Paint::Accent
            } else {
                Paint::Track
            };
            lay.fill(Fill::rounded(rect.to_array(), track, rect.height / 2.0));
            let knob = rect.height - lay.u(6.0);
            let x = if *on {
                rect.right() - lay.u(3.0) - knob
            } else {
                rect.x + lay.u(3.0)
            };
            let paint = if locked { Paint::Icon } else { Paint::Ink };
            lay.fill(Fill::rounded(
                [x, rect.y + lay.u(3.0), knob, knob],
                paint,
                knob / 2.0,
            ));
            if !locked {
                lay.hit(rect, whole);
            }
        }
        ControlView::Segmented { labels, picked } => {
            lay.fill(
                Fill::rounded(rect.to_array(), Paint::Well, lay.u(8.0))
                    .bordered(edge, Paint::PaletteEdge),
            );
            let mut x = rect.x + lay.u(3.0);
            for (at, label) in labels.iter().enumerate() {
                let is_picked = *picked == Some(at);
                let ink = match (is_picked, locked) {
                    (true, false) => Ink::OnColor,
                    (true, true) => Ink::Bold,
                    (false, _) => Ink::Muted,
                };
                let width = lay.width(label, HELP, Ink::OnColor) + lay.u(28.0);
                let button = Rect {
                    x,
                    y: rect.y + lay.u(3.0),
                    width,
                    height: rect.height - lay.u(6.0),
                };
                let hit = SettingsHit::Control(row, Part::Segment(u8::try_from(at).unwrap_or(0)));
                if is_picked {
                    let paint = if locked { Paint::Track } else { Paint::Accent };
                    lay.fill(Fill::rounded(button.to_array(), paint, lay.u(6.0)));
                } else if !locked {
                    lay.hover(
                        hit,
                        Fill::rounded(button.to_array(), Paint::ChatHover, lay.u(6.0)),
                    );
                }
                let text = lay.width(label, HELP, ink);
                lay.text_mid(x + (width - text) / 2.0, middle, label, ink, HELP);
                if !locked {
                    lay.hit(button, hit);
                }
                x += width;
            }
        }
        ControlView::Slider { at, value } => {
            let label = lay.u(52.0);
            let track = Rect {
                x: rect.x + lay.u(8.0),
                y: (middle - lay.u(2.0)).round(),
                width: rect.width - label - lay.u(14.0) - lay.u(16.0),
                height: lay.u(4.0),
            };
            lay.fill(Fill::rounded(track.to_array(), Paint::Track, lay.u(2.0)));
            let along = track.width * at.clamp(0.0, 1.0);
            let lit = if locked { Paint::Icon } else { Paint::Accent };
            lay.fill(Fill::rounded(
                [track.x, track.y, along, track.height],
                lit,
                lay.u(2.0),
            ));
            let thumb = lay.u(16.0);
            let thumb_rect = [
                (track.x + along - thumb / 2.0).round(),
                (middle - thumb / 2.0).round(),
                thumb,
                thumb,
            ];
            lay.fill(Fill::rounded(thumb_rect, lit, thumb / 2.0));
            let text = lay.width(value, VALUE, Ink::Plain);
            lay.text_mid(rect.right() - text, middle, value, Ink::Plain, VALUE);
            if !locked {
                let hit = SettingsHit::Control(row, Part::Slider(0));
                let grab = Rect {
                    x: track.x - thumb / 2.0,
                    y: rect.y - lay.u(4.0),
                    width: track.width + thumb,
                    height: rect.height + lay.u(8.0),
                };
                lay.hit(grab, hit);
                lay.laid.sliders.push((row, track));
                lay.hover(
                    hit,
                    Fill::rounded(
                        [
                            thumb_rect[0] - lay.u(4.0),
                            thumb_rect[1] - lay.u(4.0),
                            thumb + lay.u(8.0),
                            thumb + lay.u(8.0),
                        ],
                        Paint::AccentFaint,
                        thumb,
                    ),
                );
            }
        }
        ControlView::Stepper(value) => {
            lay.fill(
                Fill::rounded(rect.to_array(), Paint::Well, lay.u(8.0))
                    .bordered(edge, Paint::PaletteEdge),
            );
            let button = lay.u(32.0);
            for (x, sign, part) in [
                (rect.x, "−", Part::Less),
                (rect.right() - button, "+", Part::More),
            ] {
                let square = Rect {
                    x,
                    y: rect.y,
                    width: button,
                    height: rect.height,
                };
                let hit = SettingsHit::Control(row, part);
                let ink = if locked { Ink::Muted } else { Ink::Plain };
                let wide = lay.width(sign, CARD_TITLE, ink);
                lay.text_mid(x + (button - wide) / 2.0, middle, sign, ink, CARD_TITLE);
                if !locked {
                    lay.hover(
                        hit,
                        Fill::rounded(square.to_array(), Paint::ChatHover, lay.u(8.0)),
                    );
                    lay.hit(square, hit);
                }
            }
            let ink = if locked { Ink::Muted } else { Ink::Plain };
            let wide = lay.width(value, VALUE, ink);
            lay.text_mid(
                rect.x + (rect.width - wide) / 2.0,
                middle,
                value,
                ink,
                VALUE,
            );
        }
        ControlView::Font(name) => {
            lay.fill(
                Fill::rounded(rect.to_array(), Paint::Well, lay.u(8.0))
                    .bordered(edge, Paint::PaletteEdge),
            );
            let ink = if locked { Ink::Muted } else { Ink::Plain };
            lay.text_mid(rect.x + lay.u(12.0), middle, name, ink, VALUE);
            lay.icon(
                [rect.right() - lay.u(19.0), middle],
                14.0,
                Icon::ChevronDown,
                Paint::Icon,
            );
            if !locked {
                lay.hover(
                    whole,
                    Fill::rounded(rect.to_array(), Paint::ChatHover, lay.u(8.0)),
                );
                lay.hit(rect, whole);
            }
        }
        ControlView::Field { text, editing } => {
            let border = if *editing {
                Paint::Accent
            } else {
                Paint::PaletteEdge
            };
            lay.fill(
                Fill::rounded(rect.to_array(), Paint::Well, lay.u(8.0)).bordered(edge, border),
            );
            let shown = if *editing {
                crate::picker::drawn(text)
            } else if text.is_empty() {
                "none".to_owned()
            } else {
                text.clone()
            };
            let ink = if locked || (!*editing && text.is_empty()) {
                Ink::Muted
            } else {
                Ink::Plain
            };
            lay.text_mid(rect.x + lay.u(12.0), middle, &shown, ink, VALUE);
            if !locked {
                lay.hit(rect, whole);
            }
        }
        ControlView::Keys { keys, recording } => {
            let mut x = rect.x;
            let chip_height = rect.height;
            let chip =
                |lay: &mut Lay<'_>, x: &mut f32, text: &str, ink: Ink, font: TextFont, pad: f32| {
                    let wide = lay.width(text, font, ink) + pad;
                    let border = if ink == Ink::Accent {
                        Paint::Accent
                    } else {
                        Paint::PaletteEdge
                    };
                    lay.fill(
                        Fill::rounded([*x, rect.y, wide, chip_height], Paint::Well, lay.u(6.0))
                            .bordered(lay.geometry.edge, border),
                    );
                    lay.text_mid(*x + pad / 2.0, middle, text, ink, font);
                    *x += wide + lay.u(8.0);
                };
            match recording {
                Some(recording) => {
                    let pad = lay.u(20.0);
                    chip(lay, &mut x, recording, Ink::Accent, SMALL_VALUE, pad);
                }
                None => {
                    for keys in keys.iter().take(3) {
                        let pad = lay.u(20.0);
                        chip(lay, &mut x, keys, Ink::Plain, SMALL_VALUE, pad);
                    }
                }
            }
            let button = Rect {
                x: rect.right() - lay.width("Record…", HELP, Ink::Plain) - lay.u(28.0),
                width: lay.width("Record…", HELP, Ink::Plain) + lay.u(28.0),
                ..rect
            };
            lay.fill(
                Fill::rounded(button.to_array(), Paint::Clear, lay.u(6.0))
                    .bordered(edge, Paint::PaletteEdge),
            );
            lay.text_mid(button.x + lay.u(14.0), middle, "Record…", Ink::Plain, HELP);
            if !locked {
                lay.hover(
                    whole,
                    Fill::rounded(button.to_array(), Paint::ChatHover, lay.u(6.0)),
                );
                lay.hit(rect, whole);
            }
        }
        ControlView::Advanced => {
            lay.fill(
                Fill::rounded(rect.to_array(), Paint::AccentFaint, rect.height / 2.0)
                    .bordered(edge, Paint::AccentEdge),
            );
            lay.text_mid(
                rect.x + lay.u(14.0),
                middle,
                "in Advanced",
                Ink::Accent,
                HELP,
            );
            lay.hit(rect, whole);
        }
        ControlView::Cards(_) => {}
    }
}

/// Lays out the agents' cards, side by side, as many as four to a line,
/// in `rect`, for row `row`.
#[allow(clippy::cast_precision_loss)]
fn lay_cards(lay: &mut Lay<'_>, cards: &[Card], row: u16, rect: Rect, focused: bool, locked: bool) {
    let columns = cards.len().clamp(1, 4);
    let gap = lay.u(12.0);
    let width = ((rect.width - gap * (columns - 1) as f32) / columns as f32).floor();
    let height = lay.u(96.0);
    for (at, card) in cards.iter().enumerate() {
        let (line, column) = (at / columns, at % columns);
        let place = Rect {
            x: rect.x + column as f32 * (width + gap),
            y: rect.y + line as f32 * (height + gap),
            width,
            height,
        };
        let hit = SettingsHit::Control(row, Part::Card(u8::try_from(at).unwrap_or(0)));
        let radius = lay.u(12.0);
        // Locked, greyed: the one picked only outlined.
        let (fill, border, thickness) = match (card.picked, locked) {
            (true, false) => (Paint::Subagent, Paint::Agents, lay.u(1.5)),
            (true, true) => (Paint::Card, Paint::Track, lay.u(1.5)),
            (false, _) => (Paint::Card, Paint::CardEdge, lay.geometry.edge),
        };
        lay.fill(Fill::rounded(place.to_array(), fill, radius).bordered(thickness, border));
        if !card.picked && !locked {
            lay.hover(
                hit,
                Fill::rounded(place.to_array(), Paint::Clear, radius)
                    .bordered(lay.u(1.5), Paint::Track),
            );
        }
        if focused && card.picked && !locked {
            let ring = lay.u(3.0);
            lay.fill(
                Fill::rounded(
                    [
                        place.x - ring,
                        place.y - ring,
                        place.width + 2.0 * ring,
                        place.height + 2.0 * ring,
                    ],
                    Paint::Clear,
                    radius + ring,
                )
                .bordered(lay.u(1.5), Paint::Accent),
            );
        }
        let x = place.x + lay.u(16.0);
        let mut y = place.y + lay.u(16.0);
        let title_ink = if locked { Ink::Muted } else { Ink::Bold };
        lay.text([x, y], &card.title, title_ink, CARD_TITLE);
        // The dot, filled for the one picked.
        let dot = lay.u(18.0);
        let dot_rect = [
            place.right() - lay.u(16.0) - dot,
            y + (lay.line(CARD_TITLE) - dot) / 2.0,
            dot,
            dot,
        ];
        let dot_fill = if card.picked {
            let paint = if locked { Paint::Icon } else { Paint::Agents };
            Fill::rounded(dot_rect, paint, dot / 2.0)
        } else {
            Fill::rounded(dot_rect, Paint::Clear, dot / 2.0).bordered(lay.u(2.0), Paint::Track)
        };
        lay.fill(dot_fill);
        y += lay.line(CARD_TITLE) + lay.u(6.0);
        lay.text([x, y], &card.command, Ink::Muted, SMALL_VALUE);
        y += lay.line(SMALL_VALUE) + lay.u(6.0);
        if let Some((note, ink)) = &card.note {
            lay.text([x, y], note, *ink, SMALL);
        }
        if !locked {
            lay.hit(place, hit);
        }
    }
}

/// Lays out Appearance's sample in `rect`.
#[allow(clippy::cast_precision_loss)]
fn lay_preview(lay: &mut Lay<'_>, preview: &Preview, rect: Rect) {
    lay.fill(
        Fill::rounded(rect.to_array(), Paint::Tile, lay.u(12.0))
            .bordered(lay.geometry.edge, Paint::CardEdge),
    );
    let line = preview_line(lay, preview.scale);
    let cell = lay.geometry.metrics.cell;
    // While a size is dragged to, the terminal's font at that size.
    let (font, advance, text_height) = if (preview.scale - 1.0).abs() < 0.001 {
        (TextFont::Mono, cell[0], cell[1])
    } else {
        let percent = preview.scale * cell[0] / lay.sizes.mono_em.max(0.1) * 100.0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let font = TextFont::MonoAt(percent.round().clamp(1.0, f32::from(u16::MAX)) as u16);
        (font, cell[0] * preview.scale, lay.line(font))
    };
    let x = rect.x + lay.u(20.0);
    let mut y = rect.y + lay.u(16.0);
    for (at, runs) in preview.lines.iter().enumerate() {
        if preview.cursorline == Some(at) {
            lay.fill(Fill::solid(
                [
                    rect.x + lay.geometry.edge,
                    y,
                    rect.width - 2.0 * lay.geometry.edge,
                    line,
                ],
                Paint::CursorLine,
            ));
        }
        let top = y + ((line - text_height) / 2.0).round();
        if preview.cursor.0 == at {
            lay.fill(Fill::solid(
                [
                    (x + preview.cursor.1 as f32 * advance).round(),
                    top,
                    advance.round(),
                    text_height,
                ],
                Paint::Accent,
            ));
        }
        let mut cells = 0;
        for (text, ink) in runs {
            lay.text([x + cells as f32 * advance, top], text, *ink, font);
            cells += mono_width(text);
        }
        y += line;
    }
}

/// Lays out the font picker below its row's button, or above it if there's
/// no room below, in the page `room`; what's under it isn't drawn.
#[allow(
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    reason = "where it goes, then its line, then its fonts"
)]
fn lay_fonts(lay: &mut Lay<'_>, fonts: &FontsView, room: Rect) {
    let row = u16::try_from(fonts.row).unwrap_or(u16::MAX);
    let anchor = (lay.laid.hits.iter())
        .find(|(_, hit)| *hit == SettingsHit::Control(row, Part::Whole))
        .map_or(
            Rect {
                x: room.right() - lay.u(360.0),
                y: room.y,
                width: lay.u(360.0),
                height: 0.0,
            },
            |(rect, _)| *rect,
        );
    let cell = lay.geometry.metrics.cell;
    let row_height = (cell[1] + lay.u(8.0)).round();
    let head = lay.u(40.0);
    let width = anchor.width.max(lay.u(380.0)).min(room.width);
    let height = head + FONT_ROWS as f32 * row_height + lay.u(10.0);
    // Below its button, or above it, or if neither has room, over it,
    // between the header and the foot.
    let top = lay.laid.panel.y + header_height(&lay.geometry) + lay.u(8.0);
    let bottom = lay.laid.panel.bottom() - lay.u(52.0);
    let below = anchor.bottom() + lay.u(6.0);
    let above = anchor.y - lay.u(6.0) - height;
    let y = if below + height <= bottom {
        below
    } else if above >= top {
        above
    } else {
        (bottom - height).max(top)
    };
    let rect = Rect {
        x: (anchor.right() - width).max(room.x),
        y,
        width,
        height,
    };
    // What's under it isn't drawn: text can't be cut off, and icons are
    // drawn with the text, over everything else.
    let clear = |[x, y, w, h]: [f32; 4]| {
        x + w <= rect.x || x >= rect.right() || y + h <= rect.y || y >= rect.bottom()
    };
    (lay.laid.texts)
        .retain(|text| clear([text.origin[0], text.origin[1], text.size[0], text.size[1]]));
    (lay.laid.fills).retain(|fill| fill.icon.is_none() || clear(fill.rect));
    (lay.laid.hovers).retain(|(_, fill)| clear(fill.rect));
    let (radius, padding) = (lay.u(10.0), lay.geometry.metrics.padding);
    push_shadow(&mut lay.laid.fills, rect, radius, padding);
    lay.fill(
        Fill::rounded(rect.to_array(), Paint::Palette, lay.u(10.0))
            .bordered(lay.geometry.edge, Paint::PaletteEdge),
    );
    lay.hit(rect, SettingsHit::Panel);
    let middle = rect.y + head / 2.0;
    let x = rect.x + lay.u(14.0);
    let label = lay.text_mid(x, middle, "Font", Ink::Muted, TextFont::Ui);
    let query = crate::picker::drawn(&fonts.query);
    lay.text_mid(
        x + label + lay.u(12.0),
        middle,
        &query,
        Ink::Plain,
        TextFont::Mono,
    );
    let said = format!("{} · {}", fonts.listing, fonts.count);
    let wide = lay.width(&said, SMALL, Ink::Muted);
    lay.text_mid(
        rect.right() - lay.u(14.0) - wide,
        middle,
        &said,
        Ink::Muted,
        SMALL,
    );
    lay.fill(Fill::solid(
        [rect.x, rect.y + head, rect.width, lay.geometry.edge],
        Paint::PaletteEdge,
    ));
    let mut y = rect.y + head + lay.u(5.0);
    for (at, row) in fonts.rows.iter().enumerate() {
        let line = Rect {
            x: rect.x + lay.u(5.0),
            y,
            width: rect.width - lay.u(10.0),
            height: row_height,
        };
        let hit = SettingsHit::Font(u16::try_from(at).unwrap_or(u16::MAX));
        if fonts.selected == Some(at) {
            lay.fill(Fill::rounded(line.to_array(), Paint::Chosen, lay.u(6.0)));
        } else {
            lay.hover(
                hit,
                Fill::rounded(line.to_array(), Paint::ChatHover, lay.u(6.0)),
            );
        }
        let top = (y + (row_height - cell[1]) / 2.0).round();
        let mut cells = 0;
        let mut start = 0;
        for &(end, ink) in &row.runs {
            let text = &row.text[start..end];
            lay.text(
                [line.x + lay.u(9.0) + cells as f32 * cell[0], top],
                text,
                ink,
                TextFont::Mono,
            );
            cells += text.chars().count();
            start = end;
        }
        lay.hit(line, hit);
        y += row_height;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::Metrics;

    /// A page of one row, a slider half way along, being dragged.
    fn slider_page() -> EasyView {
        EasyView {
            query: String::new(),
            typing: false,
            page: Some(0),
            on_side: false,
            title: "Appearance".into(),
            subtitle: String::new(),
            preview: None,
            sections: vec![SectionView {
                heading: "Text".into(),
                rows: vec![RowView {
                    label: "Size".into(),
                    help: String::new(),
                    notes: Vec::new(),
                    lock: None,
                    control: ControlView::Slider {
                        at: 0.5,
                        value: "16 pt".into(),
                    },
                }],
            }],
            note: None,
            focus: None,
            top: 0,
            reveal: false,
            footer: String::new(),
            warning: false,
            fonts: None,
            sliding: Some(0),
            machine: None,
        }
    }

    #[test]
    fn a_slider_says_how_far_along_the_mouse_is_and_easy_takes_every_point() {
        let metrics = Metrics {
            cell: [10.0, 20.0],
            ui_line: 16.0,
            padding: 4.0,
        };
        let geometry = Geometry::new([1280, 800], metrics);
        let sizes = Sizes { mono_em: 8.0 };
        #[allow(clippy::cast_precision_loss)]
        let mut measure = |text: &str, _bold: bool| 8.0 * text.chars().count() as f32;
        let laid = lay_out(&slider_page(), &geometry, sizes, &mut measure);
        let (row, track) = laid.sliders[0];
        let middle = [track.x + track.width / 2.0, track.y + track.height / 2.0];
        assert_eq!(
            laid.hit(middle),
            Some(SettingsHit::Control(row, Part::Slider(500)))
        );
        // Dragged past its end, it's at its end.
        assert_eq!(laid.sliding_at(track.right() + 100.0), Some(1000));
        // Off the panel is the chrome's to say (see `Chrome::hit`).
        assert_eq!(laid.hit([2.0, 2.0]), None);
        // Every word drawn is within the panel.
        let panel = panel(&geometry);
        assert!(laid.texts.iter().all(|text| panel.contains(text.origin)));
    }

    const METRICS: Metrics = Metrics {
        cell: [10.0, 20.0],
        ui_line: 16.0,
        padding: 4.0,
    };

    fn laid_out(view: &EasyView, size: [u32; 2]) -> Laid {
        let geometry = Geometry::new(size, METRICS);
        #[allow(clippy::cast_precision_loss)]
        let mut measure = |text: &str, _bold: bool| 8.0 * text.chars().count() as f32;
        lay_out(view, &geometry, Sizes { mono_em: 8.0 }, &mut measure)
    }

    fn page_of(row: RowView) -> EasyView {
        let mut view = slider_page();
        view.sliding = None;
        view.sections[0].rows = vec![row];
        view
    }

    #[test]
    fn agents_cards_locked_are_greyed_unclickable_with_the_lock_and_notes() {
        let card = |title: &str, picked| Card {
            title: title.into(),
            command: title.to_lowercase(),
            note: None,
            picked,
        };
        let view = page_of(RowView {
            label: "Agent".into(),
            help: "Your init.lua sets this, and it wins over what's saved here.".into(),
            notes: vec![(
                "Currently codex, set since the config loaded.".into(),
                Ink::Caution,
            )],
            lock: Some("init.lua:3".into()),
            control: ControlView::Cards(vec![card("Claude", true), card("Codex", false)]),
        });
        let laid = laid_out(&view, [1280, 800]);
        assert!(
            !(laid.hits.iter()).any(|(_, hit)| matches!(hit, SettingsHit::Control(..))),
            "no card takes a click"
        );
        assert!(
            laid.hits
                .iter()
                .any(|(_, hit)| *hit == SettingsHit::Lock(0))
        );
        assert!(
            laid.hovers.is_empty()
                || !(laid.hovers.iter()).any(|(hit, _)| matches!(hit, SettingsHit::Control(..)))
        );
        let said: Vec<&str> = laid
            .texts
            .iter()
            .map(|text| text.content.as_str())
            .collect();
        assert!(said.contains(&"init.lua:3"), "{said:?}");
        assert!(
            said.iter().any(|text| text.starts_with("Currently")),
            "{said:?}"
        );
    }

    #[test]
    fn a_narrow_page_puts_a_wide_control_under_its_words() {
        let view = page_of(RowView {
            label: "Paste an image (Ctrl+Shift+Alt+V) as".into(),
            help: "A file path works with Claude Code and Codex on any machine.".into(),
            notes: Vec::new(),
            lock: None,
            control: ControlView::Segmented {
                labels: vec!["A file path", "The clipboard", "Somewhere else entirely"],
                picked: Some(0),
            },
        });
        let laid = laid_out(&view, [700, 800]);
        let control = (laid.hits.iter())
            .filter(|(_, hit)| matches!(hit, SettingsHit::Control(..)))
            .map(|(rect, _)| *rect)
            .reduce(|a, b| Rect {
                x: a.x.min(b.x),
                y: a.y.min(b.y),
                width: a.right().max(b.right()) - a.x.min(b.x),
                height: a.bottom().max(b.bottom()) - a.y.min(b.y),
            })
            .expect("the control");
        // No word of the row's overlaps it.
        for text in &laid.texts {
            let [x, y] = text.origin;
            let [w, h] = text.size;
            let row_word = ["Paste", "A file path works"]
                .iter()
                .any(|start| text.content.starts_with(start));
            if row_word {
                let apart = x + w <= control.x
                    || x >= control.right()
                    || y + h <= control.y
                    || y >= control.bottom();
                assert!(apart, "{} overlaps the control", text.content);
            }
        }
    }

    #[test]
    fn a_part_taller_than_the_room_is_cut_off_at_the_foot() {
        let view = page_of(RowView {
            label: "Tall".into(),
            help: "word ".repeat(400),
            notes: Vec::new(),
            lock: None,
            control: ControlView::Toggle(true),
        });
        let geometry = Geometry::new([1280, 500], METRICS);
        let laid = laid_out(&view, [1280, 500]);
        // The page's room ends a little above the panel's foot.
        let foot = panel(&geometry).bottom() - 44.0 - 12.0;
        let words: Vec<&Placed> = (laid.texts.iter())
            .filter(|text| text.content.starts_with("word"))
            .collect();
        assert!(!words.is_empty());
        assert!(
            words
                .iter()
                .all(|text| text.origin[1] + text.size[1] <= foot)
        );
        assert!(
            (laid.hits.iter())
                .filter(|(_, hit)| matches!(hit, SettingsHit::Row(_) | SettingsHit::Control(..)))
                .all(|(rect, _)| rect.bottom() <= foot)
        );
    }
}
