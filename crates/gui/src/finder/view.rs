//! How the finder looks: a panel in the middle of the window, over the
//! panes, as the palette is, with what's typed along its top, the things
//! matching under it, best first, with the characters matched picked out,
//! and beside them, if there's room, a preview of the one picked. Its list
//! and preview are in the terminal's font, a cell per character, so what's
//! picked out is runs of text side by side.

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
        (rows, cols): (usize, usize),
    ) -> Self {
        let rows_before = u64::try_from(rows / 3).unwrap_or(0);
        let start = line.map_or(first, |line| line.saturating_sub(rows_before).max(first));
        let skip = usize::try_from(start - first).unwrap_or(usize::MAX);
        let last = first + lines.len() as u64;
        let width = last.to_string().len();
        let shown: Vec<Row> = (lines.iter().skip(skip).take(rows).zip(start..))
            .map(|(text, number)| {
                let number = format!("{:>width$} ", number + 1);
                // Tabs as four spaces, and other controls as one.
                let mut shown = number.clone();
                for ch in text.chars() {
                    match ch {
                        '\t' => shown.push_str("    "),
                        ch if ch.is_control() => shown.push(' '),
                        ch => shown.push(ch),
                    }
                }
                let inks: Vec<Ink> = (shown.chars().enumerate())
                    .map(|(at, _)| {
                        if at < number.len() {
                            Ink::Muted
                        } else {
                            Ink::Plain
                        }
                    })
                    .collect();
                fit(&shown, &inks, cols, false)
            })
            .collect();
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

/// Where the finder's parts go, in a window of one size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Layout {
    pub(crate) panel: Rect,
    /// The line what's typed goes on.
    prompt: Rect,
    /// Where the list goes.
    list: Rect,
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
                    Paint::Picked,
                    (1.5 * padding).round(),
                ));
            }
            let origin = [self.list.x + 2.0 * padding, (y + padding / 2.0).round()];
            push_row(row, origin, cell_width, painted);
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
                    Paint::Hover,
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
}
