//! How the finder looks: a panel in the middle of the window, over the
//! panes, as the palette is, with what's typed along its top, the things
//! matching under it, best first, with the characters matched picked out,
//! and beside them, if there's room, a preview of the one picked, coloured
//! as its file's buffer is if it's open, with what a grep matched picked
//! out in its line. Its list and preview are in the terminal's font, a
//! cell per character, so what's picked out is runs of text side by side.

use clackatoo_protocol::Span;

use crate::chrome::{Geometry, Painted, push_shadow};
use crate::layout::Rect;
use crate::render::{Fill, Ink, Paint, Text, TextFont};

/// What the finder shows, for a panel of some size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FinderView {
    /// What it lists, such as "Files".
    pub(crate) title: &'static str,
    /// Where it looks, as people write it.
    pub(crate) place: Option<String>,
    /// What's typed, with a caret after it.
    pub(crate) query: String,
    /// How many things match, of how many.
    pub(crate) count: String,
    /// The things shown, best first.
    pub(crate) rows: Vec<Row>,
    /// Which of them is picked.
    pub(crate) selected: Option<usize>,
    pub(crate) preview: Option<PreviewView>,
    /// What the list says, quietly in its middle, when there's nothing
    /// in it: that nothing matches what's typed.
    pub(crate) empty: Option<String>,
}

/// One line of text, in runs of ink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) text: String,
    /// Where each run ends in `text`, in bytes, and its ink, in order.
    pub(crate) runs: Vec<(usize, Ink)>,
}

impl Row {
    /// Each run's text, ink, and how many cells in it starts.
    fn runs(&self) -> impl Iterator<Item = (&str, Ink, usize)> {
        let mut start = 0;
        let mut cells = 0;
        self.runs.iter().map(move |&(end, ink)| {
            let text = &self.text[start..end];
            let at = cells;
            start = end;
            cells += text.chars().count();
            (text, ink, at)
        })
    }
}

/// The lines of a file round the place picked, numbered, or why there are
/// none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreviewView {
    pub(crate) lines: Vec<Row>,
    /// Which of them is the place's line.
    pub(crate) at: Option<usize>,
}

impl PreviewView {
    /// The preview of `lines`, the first of them line `first` of the file,
    /// round line `line` if it's of a place, `rows` lines tall and `cols`
    /// cells wide.
    pub(crate) fn new(
        first: u64,
        lines: &[String],
        line: Option<u64>,
        size: (usize, usize),
    ) -> Self {
        Self::coloured(first, lines, line, size, &[], &[])
    }

    /// The preview of `lines`, as [`PreviewView::new`] makes it, coloured
    /// as `spans` say, in order, as bytes of the lines each followed by a
    /// line break; and with `matched`, byte ranges of line `line`, picked
    /// out as the list picks out what a grep matched. Only the lines shown
    /// are coloured.
    pub(crate) fn coloured(
        first: u64,
        lines: &[String],
        line: Option<u64>,
        (rows, cols): (usize, usize),
        spans: &[Span],
        matched: &[(usize, usize)],
    ) -> Self {
        let rows_before = u64::try_from(rows / 3).unwrap_or(0);
        let start = line.map_or(first, |line| line.saturating_sub(rows_before).max(first));
        let skip = usize::try_from(start - first).unwrap_or(usize::MAX);
        let last = first + lines.len() as u64;
        let width = last.to_string().len();
        // Where the line starts among the lines, as `spans` count.
        let mut from: usize = (lines.iter().take(skip)).map(|text| text.len() + 1).sum();
        let mut shown = Vec::with_capacity(rows.min(lines.len()));
        for (text, number) in lines.iter().skip(skip).take(rows).zip(start..) {
            let picked = if line == Some(number) { matched } else { &[] };
            let lit = spans.partition_point(|span| span.end <= from);
            let (row, inks) = numbered_line(number + 1, width, text, from, &spans[lit..], picked);
            shown.push(fit(&row, &inks, cols, false));
            from += text.len() + 1;
        }
        let at = line
            .and_then(|line| line.checked_sub(start))
            .and_then(|at| usize::try_from(at).ok())
            .filter(|&at| at < shown.len());
        Self { lines: shown, at }
    }

    /// A preview saying `why` there's nothing to show.
    pub(crate) fn note(why: String) -> Self {
        Self {
            lines: vec![Row {
                runs: vec![(why.len(), Ink::Muted)],
                text: why,
            }],
            at: None,
        }
    }
}

/// Line `number` of a preview, `text`, numbered `width` cells wide, with
/// its characters inked: those in `picked`, byte ranges of it, picked out;
/// the rest as the first of `spans`, bytes of the lines from `from` where
/// `text` starts, colour them, or plainly. Tabs are four spaces, and other
/// controls one. Its text, and each character's ink.
fn numbered_line(
    number: u64,
    width: usize,
    text: &str,
    from: usize,
    spans: &[Span],
    picked: &[(usize, usize)],
) -> (String, Vec<Ink>) {
    let mut shown = format!("{number:>width$} ");
    let mut inks = vec![Ink::Muted; shown.len()];
    let mut spans = spans.iter().peekable();
    for (at, ch) in text.char_indices() {
        let ink = if (picked.iter()).any(|&(start, end)| (start..end).contains(&at)) {
            Ink::Accent
        } else {
            while spans.next_if(|span| span.end <= from + at).is_some() {}
            match spans.peek() {
                Some(span) if span.start <= from + at => crate::colors::syntax_ink(span.kind),
                _ => Ink::Plain,
            }
        };
        let (shown_as, cells) = match ch {
            '\t' => (' ', 4),
            ch if ch.is_control() => (' ', 1),
            ch => (ch, 1),
        };
        for _ in 0..cells {
            shown.push(shown_as);
            inks.push(ink);
        }
    }
    (shown, inks)
}

/// `text`, each character in its ink in `inks`, in at most `cols` cells:
/// cut short with `…` at its start if `keep_end`, or else at its end.
pub(crate) fn fit(text: &str, inks: &[Ink], cols: usize, keep_end: bool) -> Row {
    let count = text.chars().count();
    let mut chars: Vec<(char, Ink)> = text.chars().zip(inks.iter().copied()).collect();
    if count > cols {
        let keep = cols.saturating_sub(1);
        if keep_end {
            chars.drain(..count - keep);
            chars.insert(0, ('…', Ink::Muted));
        } else {
            chars.truncate(keep);
            chars.push(('…', Ink::Muted));
        }
        chars.truncate(cols);
    }
    let mut row = Row {
        text: String::with_capacity(text.len()),
        runs: Vec::new(),
    };
    let mut ink = None;
    for (ch, next) in chars {
        if let Some(ink) = ink.filter(|&ink| ink != next) {
            row.runs.push((row.text.len(), ink));
        }
        ink = Some(next);
        row.text.push(ch);
    }
    if let Some(ink) = ink {
        row.runs.push((row.text.len(), ink));
    }
    row
}

/// `text` in at most `cols` cells of the terminal's font, a wide character
/// two: cut short with `…` if it's wider, at its start if `keep_end`, as
/// a long path keeps its last folders, or else at its end.
pub(crate) fn cut(text: &str, cols: usize, keep_end: bool) -> String {
    use crate::render::{mono_cells, mono_width};
    if mono_width(text) <= cols {
        return text.to_owned();
    }
    if cols == 0 {
        return String::new();
    }
    // Room is kept for the `…`.
    let mut width = 0;
    let mut fits = |ch: &char| {
        width += mono_cells(*ch);
        width < cols
    };
    if keep_end {
        let mut kept: Vec<char> = text.chars().rev().take_while(&mut fits).collect();
        kept.push('…');
        kept.into_iter().rev().collect()
    } else {
        let kept = text.chars().take_while(&mut fits);
        kept.chain(std::iter::once('…')).collect()
    }
}

/// Where the finder's parts go, in a window of one size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Layout {
    pub(crate) panel: Rect,
    /// The line what's typed goes on.
    prompt: Rect,
    /// Where the list goes.
    pub(crate) list: Rect,
    /// Where the preview goes, if there's room for one.
    preview: Option<Rect>,
    /// How tall a row of the list is.
    row_height: f32,
    /// How wide the title is, in the interface font, which what's typed
    /// goes after.
    pub(crate) title_width: f32,
    /// How many things the list shows, and how many cells wide.
    pub(crate) rows: usize,
    pub(crate) cols: usize,
    /// How many lines the preview shows, and how many cells wide.
    pub(crate) preview_size: Option<(usize, usize)>,
}

/// The narrowest list, in cells, that leaves room for a preview beside it.
const LIST_COLS_WITH_PREVIEW: f32 = 50.0;

impl Layout {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub(crate) fn new(geometry: &Geometry) -> Self {
        let metrics = geometry.metrics;
        let padding = metrics.padding;
        let [cell_width, cell_height] = metrics.cell;
        let [width, height] = geometry.size;
        let panel = Rect {
            width: (width * 0.86)
                .min(240.0 * cell_width)
                .min(width - 4.0 * padding)
                .max(1.0)
                .round(),
            height: (height * 0.76).min(height - 4.0 * padding).max(1.0).round(),
            x: 0.0,
            y: 0.0,
        };
        let panel = Rect {
            x: ((width - panel.width) / 2.0).round(),
            y: ((height - panel.height) / 2.0).round(),
            ..panel
        };
        // As tall as the palette's line.
        let line_height = (cell_height + 4.0 * padding).round();
        let prompt = Rect {
            height: line_height,
            ..panel
        };
        let body = Rect {
            x: panel.x + padding,
            y: panel.y + line_height + padding,
            width: panel.width - 2.0 * padding,
            height: (panel.height - line_height - 2.0 * padding).max(0.0),
        };
        let row_height = (cell_height + padding).round();
        let rows = (body.height / row_height).max(1.0) as usize;
        let cols_of = |width: f32| ((width - 4.0 * padding) / cell_width).max(1.0) as usize;
        let half = (body.width / 2.0).round();
        let (list, preview) = if half / cell_width >= LIST_COLS_WITH_PREVIEW {
            let list = Rect {
                width: half - padding,
                ..body
            };
            let preview = Rect {
                x: body.x + half,
                width: body.width - half,
                ..body
            };
            (list, Some(preview))
        } else {
            (body, None)
        };
        Self {
            panel,
            prompt,
            list,
            preview,
            row_height,
            title_width: 0.0,
            rows,
            cols: cols_of(list.width),
            preview_size: preview.map(|preview| (rows, cols_of(preview.width))),
        }
    }

    /// The row of a list of `rows` that `point` is over, if it's over one.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub(crate) fn row_at(&self, point: [f32; 2], rows: usize) -> Option<usize> {
        if !self.list.contains(point) {
            return None;
        }
        let row = ((point[1] - self.list.y) / self.row_height) as usize;
        (row < rows).then_some(row)
    }

    /// The panel's rectangles and text, showing `view`.
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn paint<'a>(
        &self,
        view: &'a FinderView,
        geometry: &Geometry,
        painted: &mut Painted<'a>,
    ) {
        let metrics = geometry.metrics;
        let padding = metrics.padding;
        let [cell_width, cell_height] = metrics.cell;
        push_shadow(&mut painted.over, self.panel, geometry.radius, padding);
        painted.over.push(
            Fill::rounded(self.panel.to_array(), Paint::Palette, geometry.radius)
                .bordered(geometry.edge, Paint::PaletteEdge),
        );
        // What it lists, quietly, then what's typed; and at the right, how
        // many match and where it looks.
        let ui_y = (self.prompt.y + (self.prompt.height - metrics.ui_line) / 2.0).round();
        let mono_y = (self.prompt.y + (self.prompt.height - cell_height) / 2.0).round();
        let mut x = self.prompt.x + 2.0 * padding;
        painted.over_texts.push(Text {
            origin: [x, ui_y],
            content: view.title,
            ink: Ink::Muted,
            font: TextFont::Ui,
        });
        x += self.title_width + 2.0 * padding;
        painted.over_texts.push(Text {
            origin: [x.round(), mono_y],
            content: &view.query,
            ink: Ink::Plain,
            font: TextFont::Mono,
        });
        let mut right = self.prompt.x + self.prompt.width - 2.0 * padding;
        for (text, gap) in [(&view.count, 0.0)]
            .into_iter()
            .chain((view.place.as_ref()).map(|place| (place, 3.0 * cell_width)))
        {
            right -= gap + text.chars().count() as f32 * cell_width;
            painted.over_texts.push(Text {
                origin: [right.round(), mono_y],
                content: text,
                ink: Ink::Muted,
                font: TextFont::Mono,
            });
        }
        painted.over.push(Fill::solid(
            [
                self.panel.x,
                self.prompt.y + self.prompt.height,
                self.panel.width,
                geometry.edge,
            ],
            Paint::PaletteEdge,
        ));

        for (index, row) in view.rows.iter().enumerate() {
            let y = self.list.y + index as f32 * self.row_height;
            if view.selected == Some(index) {
                painted.over.push(Fill::rounded(
                    [self.list.x, y, self.list.width, self.row_height],
                    Paint::Chosen,
                    (1.5 * padding).round(),
                ));
            }
            let origin = [self.list.x + 2.0 * padding, (y + padding / 2.0).round()];
            push_row(row, origin, cell_width, painted);
        }
        // Nothing listed says why, in the middle of where the list goes,
        // and the preview, which has nothing to show either.
        if let Some(empty) = view.empty.as_deref().filter(|_| view.rows.is_empty()) {
            let width = crate::render::mono_width(empty) as f32 * cell_width;
            let room = match (self.preview, &view.preview) {
                (Some(area), None) => area.x + area.width - self.list.x,
                _ => self.list.width,
            };
            painted.over_texts.push(Text {
                origin: [
                    (self.list.x + (room - width).max(0.0) / 2.0).round(),
                    (self.list.y + (self.list.height - cell_height) / 2.0).round(),
                ],
                content: empty,
                ink: Ink::Muted,
                font: TextFont::Mono,
            });
        }

        let (Some(area), Some(preview)) = (self.preview, &view.preview) else {
            return;
        };
        painted.over.push(Fill::solid(
            [area.x - padding, area.y, geometry.edge, area.height],
            Paint::PaletteEdge,
        ));
        for (index, row) in preview.lines.iter().enumerate() {
            let y = area.y + index as f32 * self.row_height;
            if preview.at == Some(index) {
                painted.over.push(Fill::solid(
                    [area.x, y, area.width, self.row_height],
                    Paint::Chosen,
                ));
            }
            let origin = [area.x + 2.0 * padding, (y + padding / 2.0).round()];
            push_row(row, origin, cell_width, painted);
        }
    }
}

/// Adds `row`'s runs as text from `origin`, a cell per character.
#[allow(clippy::cast_precision_loss)]
fn push_row<'a>(row: &'a Row, origin: [f32; 2], cell_width: f32, painted: &mut Painted<'a>) {
    for (text, ink, cells) in row.runs() {
        painted.over_texts.push(Text {
            origin: [(origin[0] + cells as f32 * cell_width).round(), origin[1]],
            content: text,
            ink,
            font: TextFont::Mono,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(row: &Row) -> Vec<(&str, Ink)> {
        row.runs().map(|(text, ink, _)| (text, ink)).collect()
    }

    #[test]
    fn a_row_is_runs_of_ink_cut_to_fit_at_either_end() {
        use Ink::{Accent, Muted, Plain};
        let inks = [Muted, Muted, Accent, Plain, Plain];
        let whole = fit("ab/cd", &inks, 10, true);
        assert_eq!(runs(&whole), [("ab", Muted), ("/", Accent), ("cd", Plain)]);
        assert_eq!(
            whole.runs().map(|(.., at)| at).collect::<Vec<_>>(),
            [0, 2, 3]
        );
        let kept_end = fit("ab/cd", &inks, 3, true);
        assert_eq!(runs(&kept_end), [("…", Muted), ("cd", Plain)]);
        let kept_start = fit("ab/cd", &inks, 4, false);
        assert_eq!(
            runs(&kept_start),
            [("ab", Muted), ("/", Accent), ("…", Muted)]
        );
        assert_eq!(fit("é", &[Plain], 0, true).text, "");
    }

    #[test]
    fn points_hit_the_rows_of_the_list_shown() {
        let metrics = crate::chrome::Metrics {
            cell: [10.0, 20.0],
            ui_line: 16.0,
            padding: 4.0,
        };
        let geometry = Geometry::new([1600, 900], metrics);
        let layout = Layout::new(&geometry);
        let [x, y] = [layout.list.x + 5.0, layout.list.y + 1.0];
        assert_eq!(layout.row_at([x, y], 3), Some(0));
        assert_eq!(layout.row_at([x, y + layout.row_height], 3), Some(1));
        assert_eq!(
            layout.row_at([x, y + 3.0 * layout.row_height], 3),
            None,
            "no row"
        );
        assert_eq!(
            layout.row_at([x, layout.prompt.y + 1.0], 3),
            None,
            "the prompt"
        );
        let preview = layout.preview.expect("room for a preview");
        assert_eq!(layout.row_at([preview.x + 5.0, y], 3), None);
    }

    #[test]
    fn a_preview_is_numbered_round_the_place_with_tabs_as_spaces() {
        let lines: Vec<String> = (0..20).map(|n| format!("\tline {n}")).collect();
        // From line 10 of the file, round its line 14, six lines tall.
        let preview = PreviewView::new(10, &lines, Some(14), (6, 80));
        assert_eq!(preview.lines[0].text, "13     line 2");
        assert_eq!(preview.at, Some(2));
        assert_eq!(preview.lines.len(), 6);
        assert_eq!(runs(&preview.lines[0])[0], ("13 ", Ink::Muted));
        // A file's own preview starts at its start.
        let preview = PreviewView::new(0, &lines, None, (3, 8));
        assert_eq!(preview.lines[0].text, " 1     …");
        assert_eq!(preview.at, None);
    }

    #[test]
    fn nothing_listed_says_why_quietly_in_the_middle_of_the_list() {
        let metrics = crate::chrome::Metrics {
            cell: [10.0, 20.0],
            ui_line: 16.0,
            padding: 4.0,
        };
        let geometry = Geometry::new([1600, 900], metrics);
        let layout = Layout::new(&geometry);
        let why = "Nothing matches ‘zz’";
        let mut view = FinderView {
            title: "Files",
            place: None,
            query: "zz▏".into(),
            count: "0 / 2".into(),
            rows: Vec::new(),
            selected: None,
            preview: None,
            empty: Some(why.into()),
        };
        let mut painted = Painted::default();
        layout.paint(&view, &geometry, &mut painted);
        let said = (painted.over_texts.iter())
            .find(|text| text.content == why)
            .expect("why it's empty");
        assert_eq!(said.ink, Ink::Muted);
        // In the middle of the panel, as there's no preview either.
        let middle = said.origin[0] + 20.0 * 10.0 / 2.0;
        let panel = layout.panel;
        assert!((middle - (panel.x + panel.width / 2.0)).abs() <= 6.0);
        assert!(said.origin[1] > layout.list.y + layout.row_height);
        // With rows to show, it isn't said.
        view.rows = vec![fit("README", &[Ink::Plain; 6], 10, true)];
        let mut painted = Painted::default();
        layout.paint(&view, &geometry, &mut painted);
        assert!(painted.over_texts.iter().all(|text| text.content != why));
    }
}
