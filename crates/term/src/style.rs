//! How a cell's text is drawn: colours, attributes and underline.

use bitflags::bitflags;

/// A colour as the application specified it.
///
/// Colours stay symbolic: the grid records *which* colour was asked for, and
/// the renderer resolves it against the current theme. A theme change then
/// recolours existing output instead of only new output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    /// The theme's default foreground or background, depending on use.
    #[default]
    Default,
    /// An entry in the 256-colour palette. 0–7 are the standard colours,
    /// 8–15 their bright variants, 16–231 a 6×6×6 cube and 232–255 a greyscale ramp.
    Indexed(u8),
    /// A 24-bit colour.
    Rgb(u8, u8, u8),
}

bitflags! {
    /// On/off text attributes set by SGR (Select Graphic Rendition).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct Attributes: u16 {
        /// SGR 1.
        const BOLD = 1 << 0;
        /// SGR 2, also called faint.
        const DIM = 1 << 1;
        /// SGR 3.
        const ITALIC = 1 << 2;
        /// SGR 5 and 6. Slow and rapid blink are not distinguished.
        const BLINK = 1 << 3;
        /// SGR 7: swap foreground and background.
        const INVERSE = 1 << 4;
        /// SGR 8: the text is present but not drawn.
        const HIDDEN = 1 << 5;
        /// SGR 9.
        const STRIKETHROUGH = 1 << 6;
    }
}

/// The shape of a cell's underline.
///
/// Kept apart from [`Attributes`] because the kinds are mutually exclusive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Underline {
    /// No underline.
    #[default]
    None,
    /// SGR 4 or 4:1.
    Single,
    /// SGR 21 or 4:2.
    Double,
    /// SGR 4:3, the wavy underline editors use for diagnostics.
    Curly,
    /// SGR 4:4.
    Dotted,
    /// SGR 4:5.
    Dashed,
}

/// Everything about how a cell is drawn apart from its text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    /// Text colour.
    pub fg: Color,
    /// Cell background colour.
    pub bg: Color,
    /// Underline colour (SGR 58). [`Color::Default`] means "same as the text".
    pub underline_color: Color,
    /// On/off attributes.
    pub attributes: Attributes,
    /// Underline shape.
    pub underline: Underline,
}

impl Style {
    /// The default style: default colours, no attributes.
    pub const DEFAULT: Self = Self {
        fg: Color::Default,
        bg: Color::Default,
        underline_color: Color::Default,
        attributes: Attributes::empty(),
        underline: Underline::None,
    };

    /// The style used for cells cleared while this style is active.
    ///
    /// Erasing keeps only the background colour. This is "background colour
    /// erase", which the `xterm-256color` terminfo entry we advertise promises
    /// (its `bce` capability). Without it, apps that paint a background by
    /// clearing (vim, htop) draw with the wrong colour.
    #[must_use]
    pub fn erased(&self) -> Self {
        Self {
            bg: self.bg,
            ..Self::DEFAULT
        }
    }
}
