//! The colours a terminal is drawn in, which programs ask about: OSC 10,
//! 11 and 12 for the text's, the background's and the cursor's, and OSC 4
//! for an entry of the 256-colour palette. Neovim asks for the background
//! as it starts, to choose between its light and dark colours, as vim,
//! bat, delta and Helix do.
//!
//! The colours are the window's, which tells the daemon them (see
//! [`Terminal::set_palette`](crate::Terminal::set_palette)), so the
//! terminal answers at once, as a program waits only a moment for an
//! answer. With none told, nothing is answered, and programs keep their
//! defaults.

/// A window's colours for text a program doesn't colour itself, and its
/// palette, each as red, green and blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Palette {
    /// The text's colour.
    pub foreground: [u8; 3],
    /// The background's.
    pub background: [u8; 3],
    /// The cursor's.
    pub cursor: [u8; 3],
    /// The 16 named colours, the palette's first. The rest are xterm's:
    /// a 6×6×6 cube and 24 greys.
    pub named: [[u8; 3]; 16],
}

impl Palette {
    /// The palette's entry `index`.
    #[must_use]
    pub fn indexed(&self, index: u8) -> [u8; 3] {
        let level = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
        match index {
            0..16 => self.named[usize::from(index)],
            16..232 => {
                let n = index - 16;
                [level(n / 36), level(n / 6 % 6), level(n % 6)]
            }
            _ => {
                let grey = 8 + (index - 232) * 10;
                [grey; 3]
            }
        }
    }
}

/// `colour` as X11 writes one, which programs read: `rgb:RRRR/GGGG/BBBB`,
/// each channel in four hex digits.
pub(crate) fn x11(colour: [u8; 3]) -> String {
    let [r, g, b] = colour.map(|channel| u16::from(channel) * 257);
    format!("rgb:{r:04x}/{g:04x}/{b:04x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        let mut named = [[0; 3]; 16];
        named[1] = [0xcc, 0x66, 0x66];
        Palette {
            foreground: [0xdd, 0xe1, 0xe6],
            background: [0x1d, 0x1f, 0x21],
            cursor: [0xff, 0xff, 0xff],
            named,
        }
    }

    #[test]
    fn the_palette_is_the_named_colours_then_xterms() {
        let palette = palette();
        assert_eq!(palette.indexed(1), [0xcc, 0x66, 0x66]);
        assert_eq!(palette.indexed(16), [0, 0, 0]);
        assert_eq!(palette.indexed(196), [255, 0, 0]);
        assert_eq!(palette.indexed(231), [255, 255, 255]);
        assert_eq!(palette.indexed(232), [8, 8, 8]);
        assert_eq!(palette.indexed(255), [238, 238, 238]);
    }

    #[test]
    fn colours_are_written_as_x11_does() {
        assert_eq!(x11([0x1d, 0x1f, 0xff]), "rgb:1d1d/1f1f/ffff");
        assert_eq!(x11([0, 0, 0]), "rgb:0000/0000/0000");
    }
}
