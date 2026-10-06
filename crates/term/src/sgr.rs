//! SGR (Select Graphic Rendition, `CSI … m`): the sequence that sets colours
//! and text attributes.

use crate::style::{Attributes, Color, Style, Underline};

/// Applies one SGR sequence's parameters to `style`.
///
/// Each item of `params` is one parameter with its colon-separated
/// subparameters. `38:5:208` arrives as a single item `[38, 5, 208]`, while
/// `38;5;208` arrives as three. Both spellings are in common use.
pub(crate) fn apply<'a>(style: &mut Style, params: impl IntoIterator<Item = &'a [u16]>) {
    let mut params = params.into_iter().peekable();
    if params.peek().is_none() {
        // `CSI m` with no parameters means `CSI 0 m`.
        *style = Style::DEFAULT;
        return;
    }

    while let Some(param) = params.next() {
        let attributes = &mut style.attributes;
        match param {
            [0] => *style = Style::DEFAULT,
            [1] => attributes.insert(Attributes::BOLD),
            [2] => attributes.insert(Attributes::DIM),
            [3] => attributes.insert(Attributes::ITALIC),
            [4] => style.underline = Underline::Single,
            [4, kind, ..] => style.underline = underline(*kind),
            [5 | 6] => attributes.insert(Attributes::BLINK),
            [7] => attributes.insert(Attributes::INVERSE),
            [8] => attributes.insert(Attributes::HIDDEN),
            [9] => attributes.insert(Attributes::STRIKETHROUGH),
            [21] => style.underline = Underline::Double,
            [22] => attributes.remove(Attributes::BOLD | Attributes::DIM),
            [23] => attributes.remove(Attributes::ITALIC),
            [24] => style.underline = Underline::None,
            [25] => attributes.remove(Attributes::BLINK),
            [27] => attributes.remove(Attributes::INVERSE),
            [28] => attributes.remove(Attributes::HIDDEN),
            [29] => attributes.remove(Attributes::STRIKETHROUGH),
            [n @ 30..=37] => style.fg = Color::Indexed(standard(*n - 30)),
            [38, ..] => {
                if let Some(color) = extended_color(param, &mut params) {
                    style.fg = color;
                }
            }
            [39] => style.fg = Color::Default,
            [n @ 40..=47] => style.bg = Color::Indexed(standard(*n - 40)),
            [48, ..] => {
                if let Some(color) = extended_color(param, &mut params) {
                    style.bg = color;
                }
            }
            [49] => style.bg = Color::Default,
            [58, ..] => {
                if let Some(color) = extended_color(param, &mut params) {
                    style.underline_color = color;
                }
            }
            [59] => style.underline_color = Color::Default,
            [n @ 90..=97] => style.fg = Color::Indexed(standard(*n - 90) + 8),
            [n @ 100..=107] => style.bg = Color::Indexed(standard(*n - 100) + 8),
            _ => log::trace!("unhandled SGR parameter {param:?}"),
        }
    }
}

/// Parses the colour that follows a 38, 48 or 58.
///
/// Handles the colon form, where everything is in `param` (`38:5:n`,
/// `38:2:r:g:b`, `38:2:colorspace:r:g:b`), and the semicolon form, where
/// the values are the parameters that follow (`38;5;n`, `38;2;r;g;b`).
/// The semicolon form consumes what it uses from `rest`.
fn extended_color<'a>(param: &[u16], rest: &mut impl Iterator<Item = &'a [u16]>) -> Option<Color> {
    match param {
        [_, 5, index] => indexed(*index),
        [_, 2, r, g, b] | [_, 2, _, r, g, b] => rgb(*r, *g, *b),
        [_] => {
            let mut next = || rest.next().and_then(|p| p.first().copied());
            match next()? {
                5 => indexed(next()?),
                2 => rgb(next()?, next()?, next()?),
                _ => None,
            }
        }
        _ => None,
    }
}

fn indexed(index: u16) -> Option<Color> {
    u8::try_from(index).ok().map(Color::Indexed)
}

fn rgb(r: u16, g: u16, b: u16) -> Option<Color> {
    Some(Color::Rgb(
        u8::try_from(r).ok()?,
        u8::try_from(g).ok()?,
        u8::try_from(b).ok()?,
    ))
}

/// One of the eight standard colours, from a match arm already limited to
/// 0..8.
fn standard(offset: u16) -> u8 {
    u8::try_from(offset).expect("standard colour offsets are 0..8")
}

fn underline(kind: u16) -> Underline {
    match kind {
        0 => Underline::None,
        2 => Underline::Double,
        3 => Underline::Curly,
        4 => Underline::Dotted,
        5 => Underline::Dashed,
        // 1, and kinds we don't know, which draw better as an underline than
        // as nothing.
        _ => Underline::Single,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses `spec` the way vte would: `;` separates parameters and `:`
    /// separates subparameters, with an empty value meaning 0.
    fn sgr(spec: &str) -> Style {
        let mut style = Style::DEFAULT;
        apply_spec(&mut style, spec);
        style
    }

    fn apply_spec(style: &mut Style, spec: &str) {
        let params: Vec<Vec<u16>> = if spec.is_empty() {
            Vec::new()
        } else {
            spec.split(';')
                .map(|p| p.split(':').map(|s| s.parse().unwrap_or(0)).collect())
                .collect()
        };
        apply(style, params.iter().map(Vec::as_slice));
    }

    #[test]
    fn attributes_set_and_reset() {
        let mut style = sgr("1;3;7;9");
        assert_eq!(
            style.attributes,
            Attributes::BOLD | Attributes::ITALIC | Attributes::INVERSE | Attributes::STRIKETHROUGH
        );
        apply_spec(&mut style, "22;27");
        assert_eq!(
            style.attributes,
            Attributes::ITALIC | Attributes::STRIKETHROUGH
        );
        apply_spec(&mut style, "");
        assert_eq!(style, Style::DEFAULT);
    }

    #[test]
    fn basic_and_bright_colours() {
        let style = sgr("31;102");
        assert_eq!(style.fg, Color::Indexed(1));
        assert_eq!(style.bg, Color::Indexed(10));
        assert_eq!(sgr("31;39").fg, Color::Default);
    }

    #[test]
    fn extended_colours_in_every_spelling() {
        assert_eq!(sgr("38;5;208").fg, Color::Indexed(208));
        assert_eq!(sgr("38:5:208").fg, Color::Indexed(208));
        assert_eq!(sgr("48;2;1;2;3").bg, Color::Rgb(1, 2, 3));
        assert_eq!(sgr("48:2:1:2:3").bg, Color::Rgb(1, 2, 3));
        assert_eq!(sgr("48:2::1:2:3").bg, Color::Rgb(1, 2, 3));
        assert_eq!(sgr("58:2::9:8:7").underline_color, Color::Rgb(9, 8, 7));
    }

    #[test]
    fn semicolon_colours_consume_only_their_own_parameters() {
        let style = sgr("38;5;1;1");
        assert_eq!(style.fg, Color::Indexed(1));
        assert!(style.attributes.contains(Attributes::BOLD));
    }

    #[test]
    fn invalid_colours_are_ignored() {
        assert_eq!(sgr("38;5;256").fg, Color::Default);
        assert_eq!(sgr("38;2;1;2").fg, Color::Default);
        assert_eq!(sgr("38;7").fg, Color::Default);
        assert_eq!(sgr("38").fg, Color::Default);
    }

    #[test]
    fn underline_kinds() {
        assert_eq!(sgr("4").underline, Underline::Single);
        assert_eq!(sgr("4:3").underline, Underline::Curly);
        assert_eq!(sgr("4:0").underline, Underline::None);
        assert_eq!(sgr("21").underline, Underline::Double);
        assert_eq!(sgr("4;24").underline, Underline::None);
    }
}
