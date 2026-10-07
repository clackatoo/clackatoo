//! Turning a cell's style into the colours it's drawn in.

use clackatoo_protocol::{Attributes, Color, Style};

/// A colour as red, green, blue and alpha, each from 0 to 1, in sRGB.
pub(crate) type Rgba = [f32; 4];

/// The colours text is drawn in when a program doesn't choose its own.
#[derive(Clone, Debug)]
pub(crate) struct Theme {
    pub(crate) foreground: Rgba,
    pub(crate) background: Rgba,
    pub(crate) cursor: Rgba,
    /// clackatoo's own colour, the cockatoo's crest, for the window's own
    /// marks such as the normal-mode frame.
    pub(crate) accent: Rgba,
    /// Text that matters less, such as the names in the bar not shown.
    pub(crate) muted: Rgba,
    /// The lines between panes.
    pub(crate) divider: Rgba,
    /// The background of the window's bars.
    pub(crate) bar: Rgba,
    /// Under the mouse.
    pub(crate) hover: Rgba,
    /// Under the mouse, on the close button.
    pub(crate) danger: Rgba,
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
            foreground: hex_rgba(0xc5_c8_c6),
            background: hex_rgba(0x1d_1f_21),
            cursor: hex_rgba(0xc5_c8_c6),
            accent: hex_rgba(0xf8_ce_57),
            muted: hex_rgba(0x80_84_88),
            divider: hex_rgba(0x3c_40_44),
            bar: hex_rgba(0x15_17_19),
            hover: hex_rgba(0x2c_30_34),
            danger: hex_rgba(0xc8_45_45),
            palette,
        }
    }
}

impl Theme {
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
