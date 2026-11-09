//! A terminal's lines as rows of the page: each row an element of runs
//! of text in one style, laid out cell by cell as the emulator lays them
//! out (`Line::to_row`), so wide characters, accents and emoji fill the
//! columns they fill in a window.
//!
//! Colours from the terminal's first sixteen, and the theme's own, are
//! classes the style sheet colours. Any other colour, from the 256-colour
//! palette or 24-bit, is set as a property of its run, as the page's
//! policy refuses `style` attributes.

use clackatoo_protocol::{
    Attributes, Color, CursorShape, ImageId, Line, Style, TerminalId, Underline, Widths,
};
use clackatoo_term::CellFlags;

use crate::ui::{Element, Node, el};

/// Where the cursor is on a row, and how it looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Its column.
    pub col: u16,
    pub shape: CursorShape,
}

/// `line`, as a row `width` cells wide, with `cursor` on it if it's
/// there: a `div` of the row's runs. Blank cells after the last written
/// one are left out, unless the cursor is past them.
///
/// `failed`, the exit status of a command that failed, if the row ends
/// its prompt's line, is marked two cells after the row's text, `✗ 2`,
/// as the window marks it: only where it fits, and only past whatever's
/// drawn. It's never part of the row's text, as the window's isn't: the
/// style sheet draws it from the span's `data-mark`, and what a style
/// sheet draws can't be selected or copied.
#[must_use]
pub fn row(
    line: &Line,
    width: usize,
    widths: Widths,
    cursor: Option<Cursor>,
    failed: Option<i32>,
) -> Element {
    let row = line.to_row(width.max(1), widths);
    let cells = row.cells();
    let blank = |col: usize| {
        let cell = cells[col];
        cell.ch() == ' ' && cell.flags().is_empty() && *row.style(cell) == Style::DEFAULT
    };
    let written = (0..cells.len())
        .rposition(|col| !blank(col))
        .map_or(0, |last| last + 1);
    let end = cursor
        .map_or(written, |cursor| written.max(usize::from(cursor.col) + 1))
        .min(cells.len());
    let mut element = el("div").class("r");
    let mut run = String::new();
    let mut run_style = Style::DEFAULT;
    for (col, &cell) in cells.iter().enumerate().take(end) {
        if cell.flags().contains(CellFlags::WIDE_SPACER) {
            continue;
        }
        let style = *row.style(cell);
        let wide = cell.flags().contains(CellFlags::WIDE);
        let at_cursor = cursor.filter(|cursor| {
            let at = usize::from(cursor.col);
            at == col || (wide && at == col + 1)
        });
        if wide || at_cursor.is_some() {
            flush(&mut element, &mut run, run_style);
            let mut text = String::new();
            row.push_text(col, &mut text);
            let mut span = span(style, at_cursor.map(|cursor| cursor.shape)).text(text);
            if wide {
                span = span.class("w");
            }
            element = element.child(span);
            continue;
        }
        if style != run_style {
            flush(&mut element, &mut run, run_style);
            run_style = style;
        }
        row.push_text(col, &mut run);
    }
    flush(&mut element, &mut run, run_style);
    if let Some(exit) = failed {
        let mark = format!("✗ {exit}");
        let col = row.text_end() + 2;
        if end <= col && col + mark.chars().count() <= cells.len() {
            // The blanks before it are the mark's too, so neither is
            // copied.
            let drawn = format!("{}{mark}", " ".repeat(col - end));
            element = element.child(el("span").class("xm").attr("data-mark", drawn));
        }
    }
    element
}

/// Adds `run`, in `style`, to `element`, and empties it.
fn flush(element: &mut Element, run: &mut String, style: Style) {
    if run.is_empty() {
        return;
    }
    let text = std::mem::take(run);
    let node: Node = if style == Style::DEFAULT {
        text.into()
    } else {
        span(style, None).text(text).into()
    };
    element.children.push(node);
}

/// What a run's text and background are drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ink {
    /// The theme's text colour.
    Text,
    /// The theme's background.
    Background,
    /// One of the 256-colour palette.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// A `span` drawing its text in `style`, as the cursor if `cursor` says
/// how it looks.
fn span(style: Style, cursor: Option<CursorShape>) -> Element {
    let block = matches!(cursor, Some(CursorShape::Block | CursorShape::Default));
    let inverse = style.attributes.contains(Attributes::INVERSE) != block;
    let ink = |color: Color, default: Ink| match color {
        Color::Default => default,
        Color::Indexed(index) => Ink::Indexed(index),
        Color::Rgb(r, g, b) => Ink::Rgb(r, g, b),
    };
    let (text, background) = if inverse {
        (ink(style.bg, Ink::Background), ink(style.fg, Ink::Text))
    } else {
        (ink(style.fg, Ink::Text), ink(style.bg, Ink::Background))
    };
    let mut span = el("span");
    span = match text {
        Ink::Text => span,
        Ink::Background => span.class("fb"),
        Ink::Indexed(index @ 0..16) => span.class(&format!("f{index}")),
        other => span.style("color", css(other)),
    };
    span = match background {
        Ink::Background => span,
        Ink::Text => span.class("bf"),
        Ink::Indexed(index @ 0..16) => span.class(&format!("b{index}")),
        other => span.style("background-color", css(other)),
    };
    let attributes = style.attributes;
    for (attribute, class) in [
        (Attributes::BOLD, "bo"),
        (Attributes::DIM, "dm"),
        (Attributes::ITALIC, "it"),
        (Attributes::HIDDEN, "hd"),
        (Attributes::STRIKETHROUGH, "st"),
        (Attributes::OVERLINE, "ol"),
    ] {
        span = span.class_if(attributes.contains(attribute), class);
    }
    span = match style.underline {
        Underline::None => span,
        Underline::Single => span.class("u1"),
        Underline::Double => span.class("u2"),
        Underline::Curly => span.class("u3"),
        Underline::Dotted => span.class("u4"),
        Underline::Dashed => span.class("u5"),
    };
    if style.underline != Underline::None && style.underline_color != Color::Default {
        let color = ink(style.underline_color, Ink::Text);
        span = span.style("text-decoration-color", css(color));
    }
    match cursor {
        None => span,
        Some(CursorShape::Bar) => span.class("cur").class("bar"),
        Some(CursorShape::Underline) => span.class("cur").class("under"),
        Some(CursorShape::Block | CursorShape::Default) => span.class("cur"),
    }
}

/// A colour outside the style sheet's, as CSS: the 256-colour palette's
/// cube and greys, or 24-bit.
fn css(ink: Ink) -> String {
    let (r, g, b) = match ink {
        Ink::Rgb(r, g, b) => (r, g, b),
        Ink::Indexed(index @ 16..=231) => {
            let level = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            let n = index - 16;
            (level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        Ink::Indexed(index @ 232..=255) => {
            let grey = 8 + (index - 232) * 10;
            (grey, grey, grey)
        }
        // The first sixteen and the theme's own are the style sheet's.
        Ink::Indexed(_) | Ink::Text | Ink::Background => return "inherit".to_owned(),
    };
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// An image a program placed over a terminal's cells (CLACKA-239).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The image, and the terminal whose it is.
    pub image: ImageId,
    pub terminal: TerminalId,
    /// Its size in pixels, which `crop` is of.
    pub size: [u32; 2],
    /// Its first row and column, from the grid's top left.
    pub row: u32,
    pub col: u32,
    /// How many rows and columns it covers.
    pub rows: u32,
    pub cols: u32,
    /// Where in its first cell it starts, in pixels of a cell `cell`
    /// pixels in size, as the program was told cells are.
    pub offset: [u32; 2],
    pub cell: [u32; 2],
    /// The part of it shown, in its pixels: left, top, width and height.
    /// `None` for all of it.
    pub crop: Option<[u32; 4]>,
    /// Over the rows' text, or under it.
    pub above: bool,
}

/// `placed`, as an element over its cells, in the grid's own units: a
/// cell is `1ch` wide and a row `1.45em` tall, as `.grid .r` is, so it
/// grows and shrinks with the rows. The image inside is shown as the
/// chats' are (see `app::images`), stretched so its crop fills it.
#[must_use]
pub fn placement(placed: &Placement) -> Element {
    let part = |at: u32, of: u32| {
        if of == 0 {
            0.0
        } else {
            f64::from(at) / f64::from(of)
        }
    };
    let left = f64::from(placed.col) + part(placed.offset[0], placed.cell[0]);
    let top = f64::from(placed.row) + part(placed.offset[1], placed.cell[1]);
    let [width, height] = placed.size;
    let [x, y, shown_w, shown_h] = placed.crop.unwrap_or([0, 0, width, height]);
    // As percentages of the cells it covers.
    let scale = |at: u32, of: u32| 100.0 * part(at, of.max(1));
    let image = el("img")
        .attr("alt", "")
        .attr("data-image", placed.image.to_string())
        .attr("data-terminal", placed.terminal.to_string())
        .style("width", format!("{}%", scale(width, shown_w)))
        .style("height", format!("{}%", scale(height, shown_h)))
        .style("left", format!("{}%", 0.0 - scale(x, shown_w)))
        .style("top", format!("{}%", 0.0 - scale(y, shown_h)));
    el("div")
        .class("placed")
        .style("left", format!("{left}ch"))
        .style("top", format!("calc({top} * 1.45em)"))
        .style("width", format!("{}ch", placed.cols))
        .style("height", format!("calc({} * 1.45em)", placed.rows))
        .style("z-index", if placed.above { "1" } else { "-1" })
        .child(image)
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::Run;

    use super::*;

    fn line(runs: &[(Style, &str)]) -> Line {
        Line {
            runs: (runs.iter())
                .map(|&(style, text)| Run {
                    style,
                    text: text.to_owned(),
                    separate: false,
                })
                .collect(),
            wrapped: false,
        }
    }

    fn red() -> Style {
        Style {
            fg: Color::Indexed(1),
            ..Style::DEFAULT
        }
    }

    #[test]
    fn runs_of_one_style_are_one_span_and_plain_text_is_text() {
        let row = row(
            &line(&[(Style::DEFAULT, "ab"), (red(), "cd")]),
            10,
            Widths::LEGACY,
            None,
            None,
        );
        assert_eq!(row.get("class"), Some("r"));
        assert_eq!(row.children.len(), 2);
        assert_eq!(row.children[0], Node::Text("ab".into()));
        let Node::Element(span) = &row.children[1] else {
            panic!("a span");
        };
        assert_eq!(span.get("class"), Some("f1"));
        assert_eq!(span.all_text(), "cd");
    }

    #[test]
    fn colours_outside_the_sixteen_are_set_on_the_run() {
        let style = Style {
            fg: Color::Rgb(1, 2, 255),
            bg: Color::Indexed(196),
            attributes: Attributes::BOLD,
            ..Style::DEFAULT
        };
        let row = row(&line(&[(style, "x")]), 4, Widths::LEGACY, None, None);
        let Node::Element(span) = &row.children[0] else {
            panic!("a span");
        };
        assert_eq!(
            span.styles,
            [
                ("color", "#0102ff".to_owned()),
                ("background-color", "#ff0000".to_owned())
            ]
        );
        assert_eq!(span.get("class"), Some("bo"));
        let overlined = Style {
            attributes: Attributes::OVERLINE | Attributes::STRIKETHROUGH,
            ..Style::DEFAULT
        };
        let row = super::row(&line(&[(overlined, "x")]), 4, Widths::LEGACY, None, None);
        let Node::Element(span) = &row.children[0] else {
            panic!("a span");
        };
        assert_eq!(span.get("class"), Some("st ol"));
        assert_eq!(css(Ink::Indexed(232)), "#080808");
        assert_eq!(css(Ink::Indexed(16)), "#000000");
    }

    #[test]
    fn the_cursor_is_its_own_cell_even_past_the_text() {
        let cursor = Cursor {
            col: 4,
            shape: CursorShape::Block,
        };
        let row = row(
            &line(&[(Style::DEFAULT, "ab")]),
            10,
            Widths::LEGACY,
            Some(cursor),
            None,
        );
        assert_eq!(row.all_text(), "ab   ");
        let Node::Element(cell) = row.children.last().unwrap() else {
            panic!("the cursor");
        };
        assert_eq!(cell.get("class"), Some("fb bf cur"), "drawn inverse");
        assert_eq!(cell.all_text(), " ");
        // On an inverse cell, a block cursor shows it the right way round.
        let inverse = Style {
            attributes: Attributes::INVERSE,
            ..Style::DEFAULT
        };
        let cursor = Cursor { col: 0, ..cursor };
        let row = super::row(
            &line(&[(inverse, "z")]),
            10,
            Widths::LEGACY,
            Some(cursor),
            None,
        );
        let Node::Element(cell) = &row.children[0] else {
            panic!("the cursor");
        };
        assert_eq!(cell.get("class"), Some("cur"));
    }

    #[test]
    fn wide_characters_take_two_columns_and_the_cursor_on_either_half() {
        let row = row(
            &line(&[(Style::DEFAULT, "字b")]),
            6,
            Widths::LEGACY,
            None,
            None,
        );
        let Node::Element(wide) = &row.children[0] else {
            panic!("a span");
        };
        assert_eq!(wide.get("class"), Some("w"));
        assert_eq!(wide.all_text(), "字");
        assert_eq!(row.children[1], Node::Text("b".into()));
        let cursor = Cursor {
            col: 1,
            shape: CursorShape::Bar,
        };
        let row = super::row(
            &line(&[(Style::DEFAULT, "字b")]),
            6,
            Widths::LEGACY,
            Some(cursor),
            None,
        );
        let Node::Element(wide) = &row.children[0] else {
            panic!("a span");
        };
        assert_eq!(wide.get("class"), Some("cur bar w"));
    }

    #[test]
    fn a_failed_command_is_marked_after_its_text_where_it_fits() {
        // The row's text, and what its mark draws after it, if anything.
        let marked = |row: &Element| {
            let mark = match row.children.last() {
                Some(Node::Element(mark)) if mark.get("class") == Some("xm") => {
                    mark.get("data-mark").map(str::to_owned)
                }
                _ => None,
            };
            (row.all_text(), mark)
        };
        let said = |text: &str, mark: Option<&str>| (text.to_owned(), mark.map(str::to_owned));
        let prompt = line(&[(Style::DEFAULT, "$ false")]);
        assert_eq!(
            marked(&row(&prompt, 20, Widths::LEGACY, None, Some(1))),
            said("$ false", Some("  ✗ 1")),
            "the blanks before it are the mark's, and neither is text"
        );
        // Exactly as wide as the row, or a cell short.
        assert_eq!(
            marked(&row(&prompt, 12, Widths::LEGACY, None, Some(1))),
            said("$ false", Some("  ✗ 1"))
        );
        assert_eq!(
            marked(&row(&prompt, 11, Widths::LEGACY, None, Some(1))),
            said("$ false", None)
        );
        assert_eq!(
            marked(&row(&prompt, 13, Widths::LEGACY, None, Some(127))),
            said("$ false", None)
        );
        // The cursor past the text, where it would go, keeps it off.
        let cursor = Cursor {
            col: 9,
            shape: CursorShape::Block,
        };
        assert_eq!(
            marked(&row(&prompt, 20, Widths::LEGACY, Some(cursor), Some(1))),
            said("$ false   ", None)
        );
        let cursor = Cursor { col: 8, ..cursor };
        assert_eq!(
            marked(&row(&prompt, 20, Widths::LEGACY, Some(cursor), Some(1))),
            said("$ false  ", Some("✗ 1"))
        );
    }

    #[test]
    fn a_blank_row_is_empty() {
        let row = row(&Line::default(), 80, Widths::LEGACY, None, None);
        assert!(row.children.is_empty());
    }

    #[test]
    fn a_placed_image_covers_its_cells_showing_its_crop() {
        let mut placed = Placement {
            image: ImageId([1; 16]),
            terminal: TerminalId(3),
            size: [400, 200],
            row: 2,
            col: 5,
            rows: 4,
            cols: 10,
            offset: [4, 10],
            cell: [8, 20],
            crop: Some([100, 50, 200, 100]),
            above: false,
        };
        let shown = placement(&placed);
        assert_eq!(shown.get("class"), Some("placed"));
        assert_eq!(
            shown.styles,
            [
                ("left", "5.5ch".to_owned()),
                ("top", "calc(2.5 * 1.45em)".to_owned()),
                ("width", "10ch".to_owned()),
                ("height", "calc(4 * 1.45em)".to_owned()),
                ("z-index", "-1".to_owned()),
            ]
        );
        let Node::Element(img) = &shown.children[0] else {
            panic!("an image");
        };
        assert_eq!(img.tag, "img");
        assert_eq!(img.get("data-image"), Some("01".repeat(16).as_str()));
        assert_eq!(img.get("data-terminal"), Some("3"));
        assert_eq!(img.get("src"), None);
        // Twice the cells' size, from half a crop to the left and above.
        assert_eq!(
            img.styles,
            [
                ("width", "200%".to_owned()),
                ("height", "200%".to_owned()),
                ("left", "-50%".to_owned()),
                ("top", "-50%".to_owned()),
            ]
        );
        placed.crop = None;
        placed.above = true;
        let shown = placement(&placed);
        assert!(shown.styles.contains(&("z-index", "1".to_owned())));
        let Node::Element(img) = &shown.children[0] else {
            panic!("an image");
        };
        assert_eq!(img.styles[0], ("width", "100%".to_owned()));
        assert_eq!(img.styles[2], ("left", "0%".to_owned()));
    }
}
