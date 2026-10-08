//! A terminal's whole state as plain data, to carry it into another
//! process: the daemon hands its terminals to a newer build of itself this
//! way when it upgrades in place.
//!
//! The types here are a file format, not a view of the emulator. They are
//! frozen: the process reading a snapshot is a *newer* build than the one
//! that wrote it, so a change to the emulator's internals mustn't change how
//! a snapshot is read. A change that needs the format to change makes a new
//! version alongside this one, and newer builds keep reading this one.
//!
//! Lines are stored as history stores them (see `scrollback`): their text,
//! the runs of it that share a style, and whether the line continues on the
//! next. That keeps everything a row shows, including wide characters and
//! graphemes, and is a fraction of the size of the cells.
//!
//! Not kept: an escape sequence or character cut off part way (the next
//! build starts with a fresh parser), unanswered replies, and damage (a
//! restored terminal is all damaged).
//!
//! What the shell marked (see [`crate::blocks`]) came later, so it is a
//! snapshot of its own, [`BlocksSnapshot`], and a terminal snapshotted
//! without one restores with no blocks.

use std::collections::VecDeque;

use crate::blocks::{Blocks, Entry, Mark, Phase as LivePhase};
use crate::charset::{Charset as LiveCharset, Charsets as LiveCharsets, Slot};
use crate::mouse::{MouseEncoding, MouseTracking};
use crate::row::{Row, RowBuilder};
use crate::style::{
    Attributes, Color as LiveColor, Style as LiveStyle, Underline as LiveUnderline,
};
use crate::terminal::{CursorShape, CursorState, Modes as LiveModes, SavedCursor, ZWJ};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A terminal's state, from [`Terminal::snapshot`](crate::Terminal::snapshot),
/// for [`Terminal::restore`](crate::Terminal::restore).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Snapshot {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) title: String,
    /// The primary screen, exactly `rows` lines of `cols` columns.
    pub(crate) primary: Vec<Line>,
    /// The primary screen's history, oldest first, each at its own width.
    pub(crate) history: Vec<Line>,
    /// The number the next line into history will get.
    pub(crate) history_end: u64,
    /// The alternate screen, if a program has ever switched to it.
    pub(crate) alternate: Option<Vec<Line>>,
    pub(crate) alternate_shown: bool,
    pub(crate) cursor: Cursor,
    pub(crate) saved_primary: SavedCursorState,
    pub(crate) saved_alternate: SavedCursorState,
    pub(crate) modes: Modes,
    /// The scroll region's first line and the line after its last.
    pub(crate) scroll_region: (u16, u16),
    /// The columns with tab stops, in order.
    pub(crate) tab_stops: Vec<u16>,
    pub(crate) cursor_shape: Shape,
    /// The character REP repeats.
    pub(crate) last_printed: Option<char>,
}

/// What a terminal's shell marked, from
/// [`Terminal::blocks_snapshot`](crate::Terminal::blocks_snapshot), for
/// [`Terminal::restore_blocks`](crate::Terminal::restore_blocks).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct BlocksSnapshot {
    /// The working directory the shell last reported.
    pub(crate) cwd: Option<String>,
    /// The primary screen's marked lines, numbered through its history and
    /// on through the screen, oldest first.
    pub(crate) marks: Vec<MarkedLine>,
    /// The number of the first block.
    pub(crate) first: u32,
    pub(crate) blocks: Vec<BlockEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct MarkedLine {
    pub(crate) line: u64,
    pub(crate) block: u32,
    pub(crate) prompt: bool,
    pub(crate) output: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct BlockEntry {
    pub(crate) command: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) phase: Phase,
    pub(crate) exit: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) enum Phase {
    Prompt,
    Running,
    Finished,
}

impl BlocksSnapshot {
    pub(crate) fn capture(
        cwd: Option<&str>,
        marks: impl Iterator<Item = (u64, Mark)>,
        blocks: &Blocks,
    ) -> Self {
        let (first, entries) = blocks.parts();
        Self {
            cwd: cwd.map(str::to_owned),
            marks: marks
                .map(|(line, mark)| MarkedLine {
                    line,
                    block: mark.block,
                    prompt: mark.prompt,
                    output: mark.output,
                })
                .collect(),
            first,
            blocks: entries
                .iter()
                .map(|entry| BlockEntry {
                    command: entry.command.clone(),
                    cwd: entry.cwd.clone(),
                    phase: match entry.phase {
                        LivePhase::Prompt => Phase::Prompt,
                        LivePhase::Running => Phase::Running,
                        LivePhase::Finished => Phase::Finished,
                    },
                    exit: entry.exit,
                })
                .collect(),
        }
    }

    /// The marks, by line, and the blocks.
    pub(crate) fn restore(&self) -> (Vec<(u64, Mark)>, Blocks) {
        let marks = self
            .marks
            .iter()
            .map(|marked| {
                let mark = Mark {
                    block: marked.block,
                    prompt: marked.prompt,
                    output: marked.output,
                };
                (marked.line, mark)
            })
            .collect();
        let entries: VecDeque<Entry> = self
            .blocks
            .iter()
            .map(|entry| Entry {
                command: entry.command.clone(),
                cwd: entry.cwd.clone(),
                phase: match entry.phase {
                    Phase::Prompt => LivePhase::Prompt,
                    Phase::Running => LivePhase::Running,
                    Phase::Finished => LivePhase::Finished,
                },
                exit: entry.exit,
            })
            .collect();
        (marks, Blocks::from_parts(self.first, entries))
    }
}

/// One row: its text, styled in runs.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Line {
    pub(crate) text: String,
    /// Text before the first run is in the default style.
    pub(crate) runs: Vec<Run>,
    /// The columns the row has.
    pub(crate) width: u16,
    /// The row's text continues on the next row. If it then fills fewer
    /// than `width` columns, the last column was padding, left because a
    /// wide character didn't fit.
    pub(crate) continues: bool,
}

/// Text from `start` (a byte offset into the line's text) up to the next
/// run is in `style`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Run {
    pub(crate) start: u32,
    pub(crate) style: Style,
    /// The first character starts a cell of its own, though the cell before
    /// ends in a ZWJ.
    pub(crate) separate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Style {
    pub(crate) fg: Color,
    pub(crate) bg: Color,
    pub(crate) underline_color: Color,
    /// Bold 1, dim 2, italic 4, blink 8, inverse 16, hidden 32,
    /// strikethrough 64.
    pub(crate) attributes: u16,
    pub(crate) underline: Underline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) enum Underline {
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Cursor {
    pub(crate) line: u16,
    pub(crate) col: u16,
    pub(crate) pending_wrap: bool,
    /// The style printed text gets.
    pub(crate) pen: Style,
    pub(crate) g0: Charset,
    pub(crate) g1: Charset,
    /// G1 is shifted in, rather than G0.
    pub(crate) g1_active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) enum Charset {
    Ascii,
    DecSpecialGraphics,
}

/// What DECSC saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct SavedCursorState {
    pub(crate) cursor: Cursor,
    pub(crate) origin: bool,
}

/// The modes set, by the numbers programs set them by: ANSI modes (SM)
/// and DEC private modes (DECSET). Reading ignores numbers it doesn't
/// know, so a mode added later needs no new version, and one this build
/// doesn't know is simply off.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Modes {
    pub(crate) ansi: Vec<u16>,
    pub(crate) dec: Vec<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) enum Shape {
    Default,
    Block,
    Underline,
    Bar,
}

/// A snapshot that can't be a terminal's state.
#[derive(Debug, thiserror::Error)]
#[error("the terminal's snapshot is damaged: {0}")]
pub struct RestoreError(pub(crate) &'static str);

// --- Lines -------------------------------------------------------------------

impl Line {
    /// `row`, which ends in padding if `padded`.
    pub(crate) fn of_row(row: &Row, padded: bool) -> Self {
        let cells = row.cells();
        // As history stores rows: a row that continues keeps every column
        // but padding; one that doesn't loses its trailing blanks.
        let end = if row.is_wrapped() {
            cells.len() - usize::from(padded)
        } else {
            cells[..row.written()]
                .iter()
                .rposition(|&cell| cell != crate::Cell::BLANK)
                .map_or(0, |last| last + 1)
        };
        let mut text = String::new();
        let mut runs = Vec::new();
        let mut current = LiveStyle::DEFAULT;
        let mut ends_in_zwj = false;
        for (col, &cell) in cells[..end].iter().enumerate() {
            if cell.is_wide_spacer() {
                continue;
            }
            let style = *row.style(cell);
            if style != current || ends_in_zwj {
                runs.push(Run {
                    start: offset(text.len()),
                    style: Style::from(style),
                    separate: ends_in_zwj,
                });
                current = style;
            }
            text.push(cell.ch());
            ends_in_zwj = false;
            if let Some(extra) = row.grapheme(col) {
                text.extend(extra);
                ends_in_zwj = extra.last() == Some(&ZWJ);
            }
        }
        Self {
            text,
            runs,
            width: u16::try_from(cells.len()).expect("rows are at most Size::MAX wide"),
            continues: row.is_wrapped(),
        }
    }

    /// The row, and whether it ends in padding.
    pub(crate) fn to_row(&self) -> Result<(Row, bool), RestoreError> {
        if self.width == 0 {
            return Err(RestoreError("a line has no columns"));
        }
        let mut builder = RowBuilder::new(usize::from(self.width));
        let text = |start: u32, end: u32| {
            self.text
                .get(span(start, end))
                .ok_or(RestoreError("a style run is out of place"))
        };
        let text_end = offset(self.text.len());
        let first = self.runs.first().map_or(text_end, |run| run.start);
        builder.push(text(0, first)?, &LiveStyle::DEFAULT, false);
        for (index, run) in self.runs.iter().enumerate() {
            let end = self.runs.get(index + 1).map_or(text_end, |next| next.start);
            builder.push(text(run.start, end)?, &run.style.into(), run.separate);
        }
        let padded = self.continues && builder.filled() < usize::from(self.width);
        Ok((builder.finish(self.continues), padded))
    }
}

/// A line or column, which fits because sizes do.
pub(crate) fn position(value: usize) -> u16 {
    u16::try_from(value).expect("screen positions fit in u16")
}

fn offset(len: usize) -> u32 {
    u32::try_from(len).expect("a line's text is far below 4 GiB")
}

fn span(start: u32, end: u32) -> std::ops::Range<usize> {
    let index = |value: u32| usize::try_from(value).expect("u32 fits in usize");
    index(start)..index(end)
}

// --- Conversions -------------------------------------------------------------

impl From<LiveStyle> for Style {
    fn from(style: LiveStyle) -> Self {
        Self {
            fg: style.fg.into(),
            bg: style.bg.into(),
            underline_color: style.underline_color.into(),
            attributes: style.attributes.bits(),
            underline: match style.underline {
                LiveUnderline::None => Underline::None,
                LiveUnderline::Single => Underline::Single,
                LiveUnderline::Double => Underline::Double,
                LiveUnderline::Curly => Underline::Curly,
                LiveUnderline::Dotted => Underline::Dotted,
                LiveUnderline::Dashed => Underline::Dashed,
            },
        }
    }
}

impl From<Style> for LiveStyle {
    fn from(style: Style) -> Self {
        Self {
            fg: style.fg.into(),
            bg: style.bg.into(),
            underline_color: style.underline_color.into(),
            attributes: Attributes::from_bits_truncate(style.attributes),
            underline: match style.underline {
                Underline::None => LiveUnderline::None,
                Underline::Single => LiveUnderline::Single,
                Underline::Double => LiveUnderline::Double,
                Underline::Curly => LiveUnderline::Curly,
                Underline::Dotted => LiveUnderline::Dotted,
                Underline::Dashed => LiveUnderline::Dashed,
            },
        }
    }
}

impl From<LiveColor> for Color {
    fn from(color: LiveColor) -> Self {
        match color {
            LiveColor::Default => Self::Default,
            LiveColor::Indexed(index) => Self::Indexed(index),
            LiveColor::Rgb(r, g, b) => Self::Rgb(r, g, b),
        }
    }
}

impl From<Color> for LiveColor {
    fn from(color: Color) -> Self {
        match color {
            Color::Default => Self::Default,
            Color::Indexed(index) => Self::Indexed(index),
            Color::Rgb(r, g, b) => Self::Rgb(r, g, b),
        }
    }
}

impl From<CursorState> for Cursor {
    fn from(cursor: CursorState) -> Self {
        let (g0, g1, active) = cursor.charsets.parts();
        let charset = |charset| match charset {
            LiveCharset::Ascii => Charset::Ascii,
            LiveCharset::DecSpecialGraphics => Charset::DecSpecialGraphics,
        };
        Self {
            line: position(cursor.line),
            col: position(cursor.col),
            pending_wrap: cursor.pending_wrap,
            pen: cursor.pen.into(),
            g0: charset(g0),
            g1: charset(g1),
            g1_active: active == Slot::G1,
        }
    }
}

impl From<Cursor> for CursorState {
    fn from(cursor: Cursor) -> Self {
        let charset = |charset| match charset {
            Charset::Ascii => LiveCharset::Ascii,
            Charset::DecSpecialGraphics => LiveCharset::DecSpecialGraphics,
        };
        let active = if cursor.g1_active { Slot::G1 } else { Slot::G0 };
        Self {
            line: usize::from(cursor.line),
            col: usize::from(cursor.col),
            pending_wrap: cursor.pending_wrap,
            pen: cursor.pen.into(),
            charsets: LiveCharsets::from_parts(charset(cursor.g0), charset(cursor.g1), active),
        }
    }
}

impl From<SavedCursor> for SavedCursorState {
    fn from(saved: SavedCursor) -> Self {
        Self {
            cursor: saved.cursor.into(),
            origin: saved.origin,
        }
    }
}

impl From<SavedCursorState> for SavedCursor {
    fn from(saved: SavedCursorState) -> Self {
        Self {
            cursor: saved.cursor.into(),
            origin: saved.origin,
        }
    }
}

/// Each mode flag, whether it is a DEC private mode, and its number.
const NUMBERED: [(LiveModes, bool, u16); 9] = [
    (LiveModes::INSERT, false, 4),
    (LiveModes::LINEFEED_NEWLINE, false, 20),
    (LiveModes::APP_CURSOR_KEYS, true, 1),
    (LiveModes::ORIGIN, true, 6),
    (LiveModes::AUTOWRAP, true, 7),
    (LiveModes::CURSOR_VISIBLE, true, 25),
    (LiveModes::APP_KEYPAD, true, 66),
    (LiveModes::FOCUS_EVENTS, true, 1004),
    (LiveModes::BRACKETED_PASTE, true, 2004),
];

impl Modes {
    pub(crate) fn capture(
        modes: LiveModes,
        tracking: MouseTracking,
        encoding: MouseEncoding,
    ) -> Self {
        let mut captured = Self {
            ansi: Vec::new(),
            dec: Vec::new(),
        };
        for (flag, dec, number) in NUMBERED {
            if modes.contains(flag) {
                if dec {
                    &mut captured.dec
                } else {
                    &mut captured.ansi
                }
                .push(number);
            }
        }
        captured.dec.extend(tracking.mode());
        captured.dec.extend(encoding.mode());
        captured
    }

    pub(crate) fn restore(&self) -> (LiveModes, MouseTracking, MouseEncoding) {
        let mut modes = LiveModes::empty();
        for (flag, dec, number) in NUMBERED {
            let set = if dec { &self.dec } else { &self.ansi };
            modes.set(flag, set.contains(&number));
        }
        let tracking = self
            .dec
            .iter()
            .find_map(|&mode| MouseTracking::from_mode(mode));
        let encoding = self
            .dec
            .iter()
            .find_map(|&mode| MouseEncoding::from_mode(mode));
        (
            modes,
            tracking.unwrap_or_default(),
            encoding.unwrap_or_default(),
        )
    }
}

impl From<CursorShape> for Shape {
    fn from(shape: CursorShape) -> Self {
        match shape {
            CursorShape::Default => Self::Default,
            CursorShape::Block => Self::Block,
            CursorShape::Underline => Self::Underline,
            CursorShape::Bar => Self::Bar,
        }
    }
}

impl From<Shape> for CursorShape {
    fn from(shape: Shape) -> Self {
        match shape {
            Shape::Default => Self::Default,
            Shape::Block => Self::Block,
            Shape::Underline => Self::Underline,
            Shape::Bar => Self::Bar,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The attribute bits are part of the format. If this fails, convert
    /// them in `From<Style>` rather than changing the snapshot.
    #[test]
    fn attribute_bits_are_as_the_format_says() {
        let bits = [
            (Attributes::BOLD, 1),
            (Attributes::DIM, 2),
            (Attributes::ITALIC, 4),
            (Attributes::BLINK, 8),
            (Attributes::INVERSE, 16),
            (Attributes::HIDDEN, 32),
            (Attributes::STRIKETHROUGH, 64),
        ];
        for (attribute, bit) in bits {
            assert_eq!(attribute.bits(), bit, "{attribute:?}");
        }
        assert_eq!(Attributes::all().bits(), 127);
    }

    #[test]
    fn runs_out_of_place_are_refused() {
        let line = Line {
            text: "é".to_owned(),
            runs: vec![Run {
                start: 1,
                style: LiveStyle::DEFAULT.into(),
                separate: false,
            }],
            width: 4,
            continues: false,
        };
        assert!(line.to_row().is_err());
    }
}
