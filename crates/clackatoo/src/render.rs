//! Turning a captured screen back into text for a terminal or a file.

use clackatoo_protocol::{Attributes, Color, GridNode, Style, Underline};

/// The screen's text, one line per row, without trailing blank lines.
pub(crate) fn plain(grid: &GridNode) -> String {
    let lines: Vec<String> = grid
        .lines
        .iter()
        .map(|line| line.text().trim_end().to_owned())
        .collect();
    join_trimmed(&lines)
}

/// The screen's text with SGR escape sequences, so colours and attributes
/// survive being printed to another terminal.
pub(crate) fn ansi(grid: &GridNode) -> String {
    let lines: Vec<String> = grid
        .lines
        .iter()
        .map(|line| {
            let mut out = String::new();
            for run in &line.runs {
                out.push_str(&sgr(&run.style));
                out.push_str(&run.text);
            }
            if !line.runs.is_empty() {
                out.push_str("\x1b[0m");
            }
            out
        })
        .collect();
    join_trimmed(&lines)
}

/// Joins lines with newlines, dropping trailing empty lines.
fn join_trimmed(lines: &[String]) -> String {
    let used = lines
        .iter()
        .rposition(|line| !line.is_empty())
        .map_or(0, |last| last + 1);
    let mut out = String::new();
    for line in &lines[..used] {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The SGR sequence that switches from any style to `style`.
fn sgr(style: &Style) -> String {
    let mut params = vec!["0".to_owned()];
    let attributes = [
        (Attributes::BOLD, "1"),
        (Attributes::DIM, "2"),
        (Attributes::ITALIC, "3"),
        (Attributes::BLINK, "5"),
        (Attributes::INVERSE, "7"),
        (Attributes::HIDDEN, "8"),
        (Attributes::STRIKETHROUGH, "9"),
    ];
    for (attribute, param) in attributes {
        if style.attributes.contains(attribute) {
            params.push(param.to_owned());
        }
    }
    let underline = match style.underline {
        Underline::None => None,
        Underline::Single => Some("4"),
        Underline::Double => Some("4:2"),
        Underline::Curly => Some("4:3"),
        Underline::Dotted => Some("4:4"),
        Underline::Dashed => Some("4:5"),
    };
    params.extend(underline.map(str::to_owned));
    params.extend(color(style.fg, 30, 90, "38"));
    params.extend(color(style.bg, 40, 100, "48"));
    params.extend(
        color(style.underline_color, 0, 0, "58")
            .filter(|_| style.underline_color != Color::Default),
    );

    let mut out = String::from("\x1b[");
    out.push_str(&params.join(";"));
    out.push('m');
    out
}

/// The SGR parameter for a colour. `base` and `bright_base` are where the
/// eight standard and eight bright colours start (30 and 90 for text), and
/// `extended` introduces 256-colour and RGB forms. `base` 0 means there's
/// no short form, as for underline colours.
fn color(color: Color, base: u8, bright_base: u8, extended: &str) -> Option<String> {
    match color {
        Color::Default => None,
        Color::Indexed(index @ 0..8) if base != 0 => Some((base + index).to_string()),
        Color::Indexed(index @ 8..16) if base != 0 => Some((bright_base + index - 8).to_string()),
        Color::Indexed(index) => Some(format!("{extended};5;{index}")),
        Color::Rgb(r, g, b) => Some(format!("{extended};2;{r};{g};{b}")),
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{CursorState, InputModes, Line, Run, Size};

    use super::*;

    fn screen(lines: Vec<Line>) -> GridNode {
        GridNode {
            size: Size::new(10, u16::try_from(lines.len()).unwrap()).unwrap(),
            cursor: CursorState {
                line: 0,
                col: 0,
                visible: true,
            },
            modes: InputModes::default(),
            lines,
        }
    }

    fn line(runs: &[(Style, &str)]) -> Line {
        Line {
            runs: runs
                .iter()
                .map(|(style, text)| Run {
                    style: *style,
                    text: (*text).to_owned(),
                    separate: false,
                })
                .collect(),
            wrapped: false,
        }
    }

    #[test]
    fn plain_text_drops_trailing_blank_lines() {
        let screen = screen(vec![
            line(&[(Style::DEFAULT, "a  ")]),
            Line::default(),
            line(&[(Style::DEFAULT, "b")]),
            Line::default(),
        ]);
        assert_eq!(plain(&screen), "a\n\nb\n");
    }

    #[test]
    fn ansi_reproduces_styles() {
        let style = Style {
            fg: Color::Indexed(9),
            bg: Color::Rgb(1, 2, 3),
            underline_color: Color::Indexed(4),
            attributes: Attributes::BOLD,
            underline: Underline::Curly,
        };
        let screen = screen(vec![line(&[(Style::DEFAULT, "a"), (style, "b")])]);
        assert_eq!(
            ansi(&screen),
            "\x1b[0ma\x1b[0;1;4:3;91;48;2;1;2;3;58;5;4mb\x1b[0m\n"
        );
    }
}
