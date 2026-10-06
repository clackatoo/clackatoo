//! Golden tests: replays real programs' recorded output and compares the
//! resulting screen with a reviewed snapshot. See `fixtures/README.md`.

use std::fmt::Write;

use clackatoo_term::recording::Recording;
use clackatoo_term::{Config, Style, Terminal};

/// Replays a fixture and describes the result: cursor, title, text, then
/// every run of non-default style.
fn replay(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}.rec", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("reading {path}: {err}"));
    let recording = Recording::parse(&bytes).expect("valid recording");
    let mut terminal = Terminal::new(recording.size, &Config::default());
    terminal.advance(recording.output);
    describe(&terminal)
}

fn describe(terminal: &Terminal) -> String {
    let cursor = terminal.cursor();
    let mut out = format!(
        "cursor: line {}, col {}{}\ntitle: {:?}\nalternate screen: {}\n\n",
        cursor.line,
        cursor.col,
        if cursor.visible { "" } else { " (hidden)" },
        terminal.title(),
        terminal.is_alternate_screen(),
    );
    for row in terminal.grid().rows() {
        let wrap = if row.is_wrapped() { "⏎" } else { "" };
        writeln!(out, "|{}|{wrap}", row.text()).unwrap();
    }

    out.push_str("\nstyles:\n");
    for (line, row) in terminal.grid().rows().enumerate() {
        let mut col = 0;
        let cells = row.cells();
        while col < cells.len() {
            let style = *row.style(cells[col]);
            let start = col;
            while col < cells.len() && *row.style(cells[col]) == style {
                col += 1;
            }
            if style != Style::DEFAULT {
                let mut text = String::new();
                for c in start..col {
                    row.push_text(c, &mut text);
                }
                writeln!(out, "  {line}:{start}..{col} {text:?} {style:?}").unwrap();
            }
        }
    }
    out
}

#[test]
fn ls_color() {
    insta::assert_snapshot!(replay("ls-color"));
}

#[test]
fn sgr_palette() {
    insta::assert_snapshot!(replay("sgr-palette"));
}

#[test]
fn vim_edit() {
    insta::assert_snapshot!(replay("vim-edit"));
}

/// Feeding a recording one byte at a time must give the same screen as
/// feeding it whole.
#[test]
fn byte_at_a_time_matches_whole() {
    for name in ["ls-color", "sgr-palette", "vim-edit"] {
        let path = format!("{}/tests/fixtures/{name}.rec", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).unwrap();
        let recording = Recording::parse(&bytes).unwrap();
        let mut terminal = Terminal::new(recording.size, &Config::default());
        for byte in recording.output {
            terminal.advance(std::slice::from_ref(byte));
        }
        assert_eq!(describe(&terminal), replay(name), "{name}");
    }
}
