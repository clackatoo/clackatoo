//! Turning a cell's style into the colours it's drawn in.

use clackatoo_protocol::{Attributes, Color, Style};

/// A colour as red, green, blue and alpha, each from 0 to 1, in sRGB.
pub(crate) type Rgba = [f32; 4];

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
    /// marks such as the focused card's edge in normal mode.
    pub(crate) accent: Rgba,
    /// The window's background fades from this, at the bottom left...
    pub(crate) backdrop: Rgba,
    /// ...to this, bluer, at the top right.
    pub(crate) backdrop_end: Rgba,
    /// A card's edge.
    pub(crate) edge: Rgba,
    /// The focused card's edge, while typing goes to its program.
    pub(crate) focus: Rgba,
    /// Behind what's picked, such as the workspace shown.
    pub(crate) picked: Rgba,
    /// Under the mouse.
    pub(crate) hover: Rgba,
    /// Text that matters less, such as the names in the bars not shown.
    pub(crate) muted: Rgba,
    /// The command palette's background.
    pub(crate) palette_background: Rgba,
    /// The command palette's edge.
    pub(crate) palette_edge: Rgba,
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
            foreground: hex_rgba(0xc8_d0_da),
            background: hex_rgba(0x11_15_1b),
            cursor: hex_rgba(0xc8_d0_da),
            accent: hex_rgba(0xd9_b2_4a),
            backdrop: hex_rgba(0x0a_0d_12),
            backdrop_end: hex_rgba(0x0e_1a_2c),
            edge: hex_rgba(0x1d_23_2c),
            focus: hex_rgba(0x2f_86_a8),
            picked: hex_rgba(0x1b_21_2a),
            hover: hex_rgba(0x18_1e_27),
            muted: hex_rgba(0x7c_86_96),
            palette_background: hex_rgba(0x14_19_20),
            palette_edge: hex_rgba(0x2a_32_3f),
            palette,
        }
    }
}

impl Theme {
    /// The colour of `paint`.
    pub(crate) fn paint(&self, paint: crate::render::Paint) -> Rgba {
        use crate::render::Paint;
        let faint = |[r, g, b, _]: Rgba, alpha: f32| [r, g, b, alpha];
        match paint {
            Paint::Clear => [0.0; 4],
            Paint::Backdrop => self.backdrop,
            Paint::BackdropEnd => self.backdrop_end,
            Paint::WindowEdge => white(0.08),
            Paint::Shadow => [0.0, 0.0, 0.0, 0.25],
            Paint::Card => self.background,
            Paint::Edge => self.edge,
            Paint::Focus => self.focus,
            Paint::FocusGlow => faint(self.focus, 0.16),
            Paint::Accent => self.accent,
            Paint::AccentGlow => faint(self.accent, 0.14),
            Paint::Picked => self.picked,
            Paint::Hover => self.hover,
            Paint::Icon => self.muted,
            Paint::Ink => self.foreground,
            Paint::Palette => self.palette_background,
            Paint::PaletteEdge => self.palette_edge,
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

    /// A program's colour, or `None` for the default.
    fn resolve(&self, color: Color) -> Option<Rgba> {
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
    fn the_cube_and_greys_match_xterm() {
        let theme = Theme::default();
        assert_eq!(theme.palette[16], hex_rgba(0x00_00_00));
        assert_eq!(theme.palette[196], hex_rgba(0xff_00_00));
        assert_eq!(theme.palette[231], hex_rgba(0xff_ff_ff));
        assert_eq!(theme.palette[232], hex_rgba(0x08_08_08));
        assert_eq!(theme.palette[255], hex_rgba(0xee_ee_ee));
    }
}
