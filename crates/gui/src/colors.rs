//! Turning a cell's style into the colours it's drawn in.

use clackatoo_protocol::{Attributes, Color, InlayHintKind, Kind, Severity, Style};
use clackatoo_text::Sign;

/// A colour as red, green, blue and alpha, each from 0 to 1, in sRGB.
pub(crate) type Rgba = [f32; 4];

/// clackatoo's own colour, the cockatoo's crest.
const ACCENT: u32 = 0xd9_b2_4a;

/// White, `alpha` opaque, for faint lines that lighten what's behind them.
const fn white(alpha: f32) -> Rgba {
    [1.0, 1.0, 1.0, alpha]
}

/// The colours text is drawn in when a program doesn't choose its own.
#[derive(Clone, Debug)]
pub(crate) struct Theme {
    pub(crate) foreground: Rgba,
    pub(crate) background: Rgba,
    pub(crate) cursor: Rgba,
    /// clackatoo's own colour, the cockatoo's crest, for the window's own
    /// marks such as highlights and the focused pane's edge.
    pub(crate) accent: Rgba,
    /// The bars' background, and the window's.
    pub(crate) bar: Rgba,
    /// The lines between the panes, and between them and the bars.
    pub(crate) divider: Rgba,
    /// Behind what's picked, such as the workspace shown...
    pub(crate) picked: Rgba,
    /// ...and its edge, in the top bar.
    pub(crate) picked_edge: Rgba,
    /// Behind the row picked in a list over the palette's background,
    /// such as the finder's: lighter than what's picked in the bars, as
    /// the palette is lighter than they are.
    pub(crate) chosen: Rgba,
    /// Under the mouse.
    pub(crate) hover: Rgba,
    /// Text that matters less, such as the names in the bars not shown.
    pub(crate) muted: Rgba,
    /// The command palette's background.
    pub(crate) palette_background: Rgba,
    /// The command palette's edge.
    pub(crate) palette_edge: Rgba,
    /// For what's gone wrong, such as the daemon being out of contact.
    pub(crate) warning: Rgba,
    /// Amber, for what's getting close to going wrong, such as a chat's
    /// context filling up.
    pub(crate) caution: Rgba,
    /// For lines added to a file...
    pub(crate) added: Rgba,
    /// ...and removed.
    pub(crate) removed: Rgba,
    /// The workspaces' colours, the dots by their names, in turn.
    pub(crate) workspaces: [Rgba; 6],
    /// The mode's colour, behind its name in the bottom bar: insert,
    /// normal and visual.
    pub(crate) modes: [Rgba; 3],
    /// The 256 indexed colours: 16 named ones, a 6×6×6 cube, and 24 greys.
    palette: [Rgba; 256],
}

/// How one cell is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CellColors {
    pub(crate) foreground: Rgba,
    /// `None` for the theme's background, which the frame is cleared to.
    pub(crate) background: Option<Rgba>,
    pub(crate) underline: Rgba,
}

impl Default for Theme {
    fn default() -> Self {
        // The 16 named colours: a dark theme, readable on its background.
        const NAMED: [u32; 16] = [
            0x1d_1f_21, 0xcc_66_66, 0xb5_bd_68, 0xf0_c6_74, 0x81_a2_be, 0xb2_94_bb, 0x8a_be_b7,
            0xc5_c8_c6, 0x66_66_66, 0xd5_4e_53, 0xb9_ca_4a, 0xe7_c5_47, 0x7a_a6_da, 0xc3_97_d8,
            0x70_c0_b1, 0xea_ea_ea,
        ];
        let mut palette = [[0.0; 4]; 256];
        for (index, &hex) in NAMED.iter().enumerate() {
            palette[index] = hex_rgba(hex);
        }
        // The standard xterm cube and grey ramp.
        let level = |n: u32| if n == 0 { 0 } else { 55 + n * 40 };
        for n in 0..216 {
            let (r, g, b) = (level(n / 36), level(n / 6 % 6), level(n % 6));
            palette[16 + n as usize] = hex_rgba(r << 16 | g << 8 | b);
        }
        for n in 0..24 {
            let grey = 8 + n * 10;
            palette[232 + n as usize] = hex_rgba(grey << 16 | grey << 8 | grey);
        }
        Self {
            foreground: hex_rgba(0xdd_e1_e6),
            background: hex_rgba(0x0c_0e_10),
            cursor: hex_rgba(0xdd_e1_e6),
            accent: hex_rgba(ACCENT),
            bar: hex_rgba(0x0012_1416),
            divider: hex_rgba(0x25_26_2c),
            picked: hex_rgba(0x1f_20_27),
            picked_edge: hex_rgba(0x2e_2f_36),
            chosen: hex_rgba(0x2f_33_3d),
            hover: hex_rgba(0x19_1b_20),
            muted: hex_rgba(0x8a_90_99),
            palette_background: hex_rgba(0x16_18_1c),
            palette_edge: hex_rgba(0x2e_2f_36),
            warning: hex_rgba(0xe0_6c_75),
            // Further from the accent's gold than the palette's yellow.
            caution: hex_rgba(0xe8_9a_3c),
            // The bright green and red.
            added: hex_rgba(NAMED[10]),
            removed: hex_rgba(NAMED[9]),
            workspaces: [
                hex_rgba(0xf8_cd_5a),
                hex_rgba(0x7c_b7_f5),
                hex_rgba(0xd5_9c_f1),
                hex_rgba(0x8f_d1_8a),
                hex_rgba(0xf0_93_7a),
                hex_rgba(0x6f_d3_c9),
            ],
            modes: [
                hex_rgba(0xc8_9c_ec),
                hex_rgba(0xe8_c2_5a),
                hex_rgba(0x7c_b7_f5),
            ],
            palette,
        }
    }
}

/// How text of `kind` is drawn in an editor. The colours are the palette's
/// named ones where there's one to suit, so they follow a terminal's theme,
/// with orange and grey for numbers and comments, as in Tomorrow Night,
/// which the default palette is.
#[must_use]
pub(crate) const fn syntax(kind: Kind) -> Style {
    const RED: Color = Color::Indexed(1);
    const GREEN: Color = Color::Indexed(2);
    const YELLOW: Color = Color::Indexed(3);
    const BLUE: Color = Color::Indexed(4);
    const MAGENTA: Color = Color::Indexed(5);
    const CYAN: Color = Color::Indexed(6);
    const ORANGE: Color = Color::Rgb(0xde, 0x93, 0x5f);
    const GREY: Color = Color::Rgb(0x96, 0x98, 0x96);
    let fg = match kind {
        Kind::Comment => GREY,
        Kind::Keyword => MAGENTA,
        Kind::String => GREEN,
        Kind::StringSpecial
        | Kind::Escape
        | Kind::Macro
        | Kind::Attribute
        | Kind::PunctuationSpecial => CYAN,
        Kind::Number | Kind::Constant => ORANGE,
        Kind::Function => BLUE,
        Kind::Type | Kind::Constructor => YELLOW,
        Kind::VariableBuiltin | Kind::Label | Kind::Tag => RED,
        Kind::Module
        | Kind::Variable
        | Kind::Parameter
        | Kind::Property
        | Kind::Operator
        | Kind::Punctuation => Color::Default,
    };
    Style {
        fg,
        ..Style::DEFAULT
    }
}

/// The tick by a lesson's step that's done: the bright green its title
/// is lit in.
#[must_use]
pub(crate) const fn lesson_done() -> Style {
    Style {
        fg: Color::Indexed(10),
        attributes: Attributes::BOLD,
        ..Style::DEFAULT
    }
}

/// The colour of the wavy line under text a diagnostic of `severity` is
/// about: the palette's red, yellow, blue and cyan, so it follows the
/// theme, as the syntax colours do.
#[must_use]
pub(crate) const fn diagnostic(severity: Severity) -> Color {
    Color::Indexed(match severity {
        Severity::Error => 1,
        Severity::Warning => 3,
        Severity::Info => 4,
        Severity::Hint => 6,
    })
}

/// How far what's wrong with a line, written after its end, stands from
/// the background, as contrast is counted (see [`contrast`]): readable at
/// a glance, past the 4.5:1 the Web's guidelines ask of text, but well
/// short of code's, which is nearer 15:1.
const VIRTUAL_TEXT_CONTRAST: f32 = 5.2;

/// How what's wrong with a line is written after its end: in the colour
/// [`Theme::virtual_text`] gives how much it matters, the theme's worked
/// out once, as rows carry their colours in styles.
#[must_use]
pub(crate) fn virtual_text(severity: Severity) -> Style {
    static COLORS: std::sync::LazyLock<[Color; 4]> = std::sync::LazyLock::new(|| {
        let theme = Theme::default();
        let color = |severity| {
            let [r, g, b, _] = theme.virtual_text(severity).map(|channel| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let byte = (channel * 255.0).round() as u8;
                byte
            });
            Color::Rgb(r, g, b)
        };
        [
            color(Severity::Error),
            color(Severity::Warning),
            color(Severity::Info),
            color(Severity::Hint),
        ]
    });
    let index = match severity {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
        Severity::Hint => 3,
    };
    Style {
        fg: COLORS[index],
        ..Style::DEFAULT
    }
}

/// How an inlay hint is written between a line's text: in the palette's
/// grey, italic, so it reads as a note on the code rather than code; a
/// parameter's name a little dimmer than a type, as it's said more often.
#[must_use]
pub(crate) const fn inlay_hint(kind: InlayHintKind) -> Style {
    let attributes = match kind {
        InlayHintKind::Parameter => Attributes::ITALIC.union(Attributes::DIM),
        InlayHintKind::Type | InlayHintKind::Other => Attributes::ITALIC,
    };
    Style {
        fg: Color::Indexed(8),
        attributes,
        ..Style::DEFAULT
    }
}

/// The sign beside a line git says is added, changed or deleted after,
/// and how it's drawn: a thin bar down the line's sign column, in the
/// palette's green or blue, or a thin red line along its foot, or for
/// lines deleted at the top, its head, as gitsigns.nvim draws them.
#[must_use]
pub(crate) const fn sign(sign: Sign) -> (&'static str, Style) {
    let (text, colour) = match sign {
        Sign::Added => ("▎", 2),
        Sign::Changed => ("▎", 4),
        Sign::Deleted => ("▁", 1),
        Sign::DeletedAbove => ("▔", 1),
    };
    let style = Style {
        fg: Color::Indexed(colour),
        ..Style::DEFAULT
    };
    (text, style)
}

/// The colours agents' changes are drawn in, a chat each, in turn: the
/// palette's magenta, cyan and blue, and their bright kinds, which git's
/// green, blue and red signs beside them don't use as they are.
const AGENT_COLOURS: [u8; 6] = [5, 6, 13, 14, 12, 4];

/// The colour of the changes chat `chat`'s agent made, from the palette,
/// so it follows the theme.
#[must_use]
pub(crate) const fn agent(chat: u32) -> Color {
    Color::Indexed(AGENT_COLOURS[chat as usize % AGENT_COLOURS.len()])
}

/// The sign beside a line an agent changed and you haven't reviewed, and
/// how it's drawn: a bar down the line in its chat's colour, or for lines
/// it deleted, a line along the foot of the line above, or the head of
/// the first, as git's signs are drawn. Kept, under `:Blame`, it's a
/// faint dot.
#[must_use]
pub(crate) const fn agent_sign(
    chat: u32,
    deleted: bool,
    top: bool,
    kept: bool,
) -> (&'static str, Style) {
    let text = match (kept, deleted, top) {
        (true, ..) => "·",
        (false, true, true) => "▔",
        (false, true, false) => "▁",
        (false, false, _) => "▍",
    };
    let style = Style {
        fg: agent(chat),
        attributes: if kept {
            Attributes::DIM
        } else {
            Attributes::empty()
        },
        ..Style::DEFAULT
    };
    (text, style)
}

/// How who changed a line is written after its end under `:Blame`: in
/// the chat's colour, dimmed, as diagnostics are.
#[must_use]
pub(crate) const fn blame(chat: u32) -> Style {
    Style {
        fg: agent(chat),
        attributes: Attributes::DIM,
        ..Style::DEFAULT
    }
}

/// The mark beside a file in an explorer git says something of, and how
/// it's drawn: its letter, as `git status` writes it, in the colours of
/// the signs beside lines, green for what's new, blue for what's changed
/// and red for what's gone or in conflict.
#[must_use]
pub(crate) const fn git_mark(mark: clackatoo_protocol::GitMark) -> (&'static str, Style) {
    use clackatoo_protocol::GitMark;
    let (text, colour) = match mark {
        GitMark::Modified => ("M", 4),
        GitMark::Added => ("A", 2),
        GitMark::Untracked => ("?", 2),
        GitMark::Deleted => ("D", 1),
        GitMark::Conflicted => ("!", 1),
    };
    let style = Style {
        fg: Color::Indexed(colour),
        ..Style::DEFAULT
    };
    (text, style)
}

/// How an editor's line numbers are drawn: faint, but the cursor's line in
/// clackatoo's accent, so the eye finds it.
#[must_use]
pub(crate) const fn line_number(cursor: bool) -> Style {
    let fg = if cursor {
        let [_, r, g, b] = ACCENT.to_be_bytes();
        Color::Rgb(r, g, b)
    } else {
        Color::Rgb(0x4f, 0x54, 0x5c)
    };
    Style {
        fg,
        ..Style::DEFAULT
    }
}

impl Theme {
    /// The colour of `paint`.
    pub(crate) fn paint(&self, paint: crate::render::Paint) -> Rgba {
        use crate::render::Paint;
        let faint = |[r, g, b, _]: Rgba, alpha: f32| [r, g, b, alpha];
        match paint {
            Paint::Clear => [0.0; 4],
            Paint::Bar => self.bar,
            Paint::WindowEdge => white(0.08),
            Paint::Shadow => [0.0, 0.0, 0.0, 0.25],
            Paint::Tile => self.background,
            Paint::Divider => self.divider,
            Paint::FocusEdge | Paint::Selected => faint(self.accent, 0.35),
            Paint::Accent => self.accent,
            Paint::Picked => self.picked,
            Paint::PickedEdge => self.picked_edge,
            Paint::Chosen => self.chosen,
            Paint::Workspace(index) => self.workspaces[usize::from(index) % self.workspaces.len()],
            Paint::Mode(mode) => self.modes[mode.colour()],
            Paint::Hover => self.hover,
            Paint::Icon | Paint::Gauge => self.muted,
            Paint::Ink => self.foreground,
            Paint::Palette => self.palette_background,
            Paint::PaletteEdge => self.palette_edge,
            Paint::Warning => faint(self.warning, 0.14),
            Paint::WarningEdge => faint(self.warning, 0.4),
            Paint::Raised => white(0.07),
            Paint::RaisedEdge => white(0.09),
            Paint::Code => white(0.05),
            Paint::Added => faint(self.added, 0.13),
            Paint::Removed => faint(self.removed, 0.13),
            Paint::Caution => self.caution,
            Paint::Full => self.warning,
        }
    }

    /// The colour of a highlight of `kind`: translucent, so the text's own
    /// colours show through.
    pub(crate) fn highlight(&self, kind: crate::render::HighlightKind) -> Rgba {
        use crate::render::HighlightKind;
        let faint = |[r, g, b, _]: Rgba, alpha: f32| [r, g, b, alpha];
        match kind {
            HighlightKind::Selection => faint(self.accent, 0.35),
            HighlightKind::Match | HighlightKind::Copied => faint(self.accent, 0.6),
            // Only just there: they say where things are, not what they are.
            HighlightKind::CursorLine => white(0.035),
            HighlightKind::Added => faint(self.added, 0.13),
            HighlightKind::Removed => faint(self.removed, 0.13),
            HighlightKind::ColorColumn => white(0.05),
            HighlightKind::Link => self.accent,
            HighlightKind::LessonStep => white(0.04),
            HighlightKind::LessonDone => faint(self.added, 0.14),
            // As the chat's code is, a little stronger, to show on the
            // step being done too.
            HighlightKind::Code => white(0.065),
            // Quieter than a search's match: the cursor merely rests there.
            HighlightKind::Used => white(0.09),
            HighlightKind::Written => faint(self.accent, 0.22),
        }
    }

    /// The colours for a cell in `style`.
    pub(crate) fn cell(&self, style: &Style) -> CellColors {
        let attributes = style.attributes;
        let mut foreground = self.resolve(style.fg).unwrap_or(self.foreground);
        let mut background = self.resolve(style.bg);
        if attributes.contains(Attributes::INVERSE) {
            let old_foreground = foreground;
            foreground = background.unwrap_or(self.background);
            background = Some(old_foreground);
        }
        if attributes.contains(Attributes::DIM) {
            foreground = mix(foreground, background.unwrap_or(self.background), 0.4);
        }
        if attributes.contains(Attributes::HIDDEN) {
            foreground = background.unwrap_or(self.background);
        }
        let underline = self.resolve(style.underline_color).unwrap_or(foreground);
        CellColors {
            foreground,
            background,
            underline,
        }
    }

    /// The colour what's wrong with a line is written in after its end,
    /// for a diagnostic of `severity`: its colour, the palette's red,
    /// yellow, blue or cyan, taken towards the background a hundredth at a
    /// time while it stays [`VIRTUAL_TEXT_CONTRAST`]:1 from it, so each is
    /// as quiet as the others. One no further than that already stays as
    /// it is: dimming all alike took the red, darkest of them, to 2.6:1.
    pub(crate) fn virtual_text(&self, severity: Severity) -> Rgba {
        let full = self
            .resolve(diagnostic(severity))
            .unwrap_or(self.foreground);
        let mut kept = full;
        for step in 1..=100_u8 {
            let toned = mix(full, self.background, f32::from(step) / 100.0);
            if contrast(toned, self.background) < VIRTUAL_TEXT_CONTRAST {
                break;
            }
            kept = toned;
        }
        kept
    }

    /// A program's colour, or `None` for the default.
    pub(crate) fn resolve(&self, color: Color) -> Option<Rgba> {
        match color {
            Color::Default => None,
            Color::Indexed(index) => Some(self.palette[usize::from(index)]),
            Color::Rgb(r, g, b) => Some(rgb(r, g, b)),
        }
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    [
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        1.0,
    ]
}

fn hex_rgba(hex: u32) -> Rgba {
    let [_, r, g, b] = hex.to_be_bytes();
    rgb(r, g, b)
}

/// `a` moved `amount` of the way towards `b`.
fn mix(a: Rgba, b: Rgba, amount: f32) -> Rgba {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * amount)
}

/// How much lighter `a` is than `b`, or `b` than `a`, as the Web's
/// guidelines on contrast count it: from 1 to 21.
pub(crate) fn contrast(a: Rgba, b: Rgba) -> f32 {
    let luminance = |rgba: Rgba| {
        let linear = |c: f32| {
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(rgba[0]) + 0.7152 * linear(rgba[1]) + 0.0722 * linear(rgba[2])
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[cfg(test)]
// The colours compared are computed exactly, from the same bytes.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn inverse_swaps_the_colours() {
        let theme = Theme::default();
        let style = Style {
            fg: Color::Indexed(1),
            attributes: Attributes::INVERSE,
            ..Style::DEFAULT
        };
        let colors = theme.cell(&style);
        assert_eq!(colors.foreground, theme.background);
        assert_eq!(colors.background, Some(theme.palette[1]));
    }

    #[test]
    fn whats_wrong_after_a_line_is_readable_but_quieter_than_code() {
        let theme = Theme::default();
        let code = contrast(theme.foreground, theme.background);
        for severity in [
            Severity::Error,
            Severity::Warning,
            Severity::Info,
            Severity::Hint,
        ] {
            let said = theme.cell(&virtual_text(severity)).foreground;
            let against = contrast(said, theme.background);
            assert!(against >= 4.5, "{severity:?} at {against}:1");
            assert!(against < code / 2.0, "{severity:?} at {against}:1");
            // Its colour is the severity's, as its underline is, taken
            // only as far towards the background as reaches 5.2:1.
            let full = theme.resolve(diagnostic(severity)).unwrap();
            let kept: Vec<f32> = (0..3)
                .map(|i| (said[i] - theme.background[i]) / (full[i] - theme.background[i]))
                .collect();
            assert!(
                kept.iter().all(|share| (share - kept[0]).abs() < 0.03),
                "{severity:?}: {kept:?}"
            );
            if contrast(full, theme.background) >= VIRTUAL_TEXT_CONTRAST {
                assert!(
                    (against - VIRTUAL_TEXT_CONTRAST).abs() < 0.15,
                    "{severity:?}"
                );
            }
        }
        // Dimming alike, as before, took the red to 2.6:1.
        let dimmed = theme.cell(&Style {
            fg: diagnostic(Severity::Error),
            attributes: Attributes::DIM,
            ..Style::DEFAULT
        });
        assert!(contrast(dimmed.foreground, theme.background) < 3.0);
    }

    #[test]
    fn the_cube_and_greys_match_xterm() {
        let theme = Theme::default();
        assert_eq!(theme.palette[16], hex_rgba(0x00_00_00));
        assert_eq!(theme.palette[196], hex_rgba(0xff_00_00));
        assert_eq!(theme.palette[231], hex_rgba(0xff_ff_ff));
        assert_eq!(theme.palette[232], hex_rgba(0x08_08_08));
        assert_eq!(theme.palette[255], hex_rgba(0xee_ee_ee));
    }
}
