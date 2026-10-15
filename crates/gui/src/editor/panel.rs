//! The panel an editor shows by its cursor: what the language server says
//! is there, as `K` asks, what's wrong there in full, and a hunk's change.
//!
//! What's shown is made once, from markdown or plain text, into
//! paragraphs of runs of text, each in one ink. They're wrapped to the
//! panel's width when it's placed, and again only if the pane's width
//! changes. Everything is in the terminal's font, a cell per character,
//! or two for a wide one, so the panel lines up with the text beside it.
//!
//! The code in what the language server says is drawn plain at first,
//! and made again in its colours once the daemon has read it (see
//! [`Panel::colour`]).

use clackatoo_text::{Diagnostic, Severity, Span};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::colors;
use crate::render::{Ink, mono_cells, mono_width};

/// The widest the panel's text goes, in cells: about as wide as prose
/// reads well.
const MAX_WIDTH: usize = 80;

/// The most lines shown at once. Longer is scrolled.
const MAX_HEIGHT: usize = 20;

/// A stretch of text in one ink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) text: String,
    pub(crate) ink: Ink,
    /// Whether it's code within prose, which has a faint box behind it.
    pub(crate) code: bool,
}

/// One line of the panel, wrapped to its width.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) runs: Vec<Run>,
    /// Whether it's a line of a block of code, which has a faint band
    /// behind it, across the panel.
    pub(crate) block: bool,
    /// Whether it's what's picked, as a completion menu's item is, which
    /// is lit behind, across the panel.
    pub(crate) picked: bool,
}

impl Line {
    /// Its text, for tests.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }
}

/// A paragraph, before it's wrapped.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Block {
    /// Prose, wrapped between words. A `\n` in a run breaks the line.
    Prose(Vec<Run>),
    /// A line of code, in runs of inks, wrapped wherever it reaches the
    /// edge.
    Code(Vec<Run>),
    /// A line across, between what the server says and what's wrong.
    Rule,
    /// An empty line, between paragraphs.
    Gap,
}

/// What the panel shows, and where.
#[derive(Debug)]
pub(crate) struct Panel {
    blocks: Vec<Block>,
    /// How wide the widest block is, unwrapped.
    widest: usize,
    /// The blocks wrapped, and the width they're wrapped to.
    lines: Vec<Line>,
    wrapped_to: usize,
    /// The first line shown.
    scroll: usize,
    /// Where it goes, as last settled, if there's room for it.
    placed: Option<Placed>,
    /// Where it was asked for: moving the cursor from there closes it.
    pub(crate) anchor: crate::motion::Point,
    /// What the language server said in it, if it says anything.
    pub(crate) hovered: Option<Hovered>,
}

/// What the language server said is at a place, to show.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Hover<'a> {
    /// What it said...
    pub(crate) text: &'a str,
    /// ...as markdown, or else plain text.
    pub(crate) markdown: bool,
    /// The query it answered, whose colours may come after.
    pub(crate) query: u64,
    /// The bytes of the buffer's text it's about, if it said.
    pub(crate) range: Option<(usize, usize)>,
}

/// What the language server said a panel shows: kept so its code can be
/// coloured once the colours come, with the text it's about, which is
/// underlined while the panel's shown.
#[derive(Debug)]
pub(crate) struct Hovered {
    /// The query it answered.
    pub(crate) query: u64,
    /// The bytes of the buffer's text it's about, if the server said,
    /// moved with edits from elsewhere, and forgotten once one changes
    /// them.
    pub(crate) range: Option<(usize, usize)>,
    /// What it said, if it's markdown, whose code may be coloured.
    markdown: Option<String>,
    /// Where its blocks start among the panel's.
    first: usize,
    tabstop: usize,
}

/// Where the panel goes in its pane, in cells: its box, which is a cell
/// wider than its text on each side, and half a cell taller above and
/// below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    /// The box's top row and left column.
    pub(crate) top: usize,
    pub(crate) left: usize,
    /// The text's width and how many of its lines are shown.
    pub(crate) width: usize,
    pub(crate) height: usize,
}

/// The panel as it's to be drawn.
#[derive(Debug)]
pub(crate) struct PanelView<'a> {
    pub(crate) placed: Placed,
    /// The lines shown.
    pub(crate) lines: &'a [Line],
    /// The first shown, and how many there are in all, for its scroll
    /// bar.
    pub(crate) first: usize,
    pub(crate) total: usize,
}

impl Panel {
    /// A panel showing `diagnostics`, in full, then `hover`, what the
    /// language server said, as markdown if it says so, with a line
    /// between. Tabs in code and plain text go to the next multiple of
    /// `tabstop` columns. `None` if there's nothing to show.
    pub(crate) fn new(
        diagnostics: &[&Diagnostic],
        hover: Option<Hover<'_>>,
        anchor: crate::motion::Point,
        tabstop: usize,
    ) -> Option<Self> {
        let mut blocks = Vec::new();
        for diagnostic in diagnostics {
            if !blocks.is_empty() {
                blocks.push(Block::Gap);
            }
            blocks.extend(described(diagnostic));
        }
        let hovered = hover.map(|hover| {
            if !blocks.is_empty() {
                blocks.push(Block::Rule);
            }
            let first = blocks.len();
            if hover.markdown {
                blocks.extend(from_markdown(hover.text, tabstop, &[]));
            } else {
                let lines = hover.text.lines().map(|line| expand_tabs(line, tabstop));
                blocks.extend(lines.map(|line| Block::Prose(vec![plain(&line)])));
            }
            Hovered {
                query: hover.query,
                range: hover.range,
                markdown: hover.markdown.then(|| hover.text.to_owned()),
                first,
                tabstop,
            }
        });
        let mut panel = Self::of(blocks, anchor)?;
        panel.hovered = hovered;
        Some(panel)
    }

    /// A panel showing `blocks`, `None` if they show nothing.
    fn of(mut blocks: Vec<Block>, anchor: crate::motion::Point) -> Option<Self> {
        trim(&mut blocks);
        (!blocks.is_empty()).then(|| Self {
            widest: (blocks.iter()).map(Block::natural_width).max().unwrap_or(0),
            blocks,
            lines: Vec::new(),
            wrapped_to: 0,
            scroll: 0,
            placed: None,
            anchor,
            hovered: None,
        })
    }

    /// Colours the code in what the language server said, if it's its
    /// answer to query `query`, as `spans`, bytes of its text, say: it's
    /// made again, and wrapped again before it's next drawn, scrolled as
    /// it was. Returns whether it was.
    pub(crate) fn colour(&mut self, query: u64, spans: &[Span]) -> bool {
        let Some(Hovered {
            markdown: Some(text),
            first,
            tabstop,
            ..
        }) = self
            .hovered
            .as_ref()
            .filter(|hovered| hovered.query == query)
        else {
            return false;
        };
        self.blocks.truncate(*first);
        self.blocks.extend(from_markdown(text, *tabstop, spans));
        trim(&mut self.blocks);
        self.widest = (self.blocks.iter())
            .map(Block::natural_width)
            .max()
            .unwrap_or(0);
        self.wrapped_to = 0;
        true
    }

    /// A panel showing a hunk as a diff: its lines as they were, `base`,
    /// in red after a `-`, then as they are, `now`, in green after a `+`,
    /// as gitsigns.nvim previews one. Tabs go to the next multiple of
    /// `tabstop` columns. `None` if both are empty.
    pub(crate) fn hunk(
        base: &str,
        now: &str,
        anchor: crate::motion::Point,
        tabstop: usize,
    ) -> Option<Self> {
        let lines = |text: &str, mark: char, ink: Ink| -> Vec<Block> {
            (text.lines())
                .map(|line| code(format!("{mark}{}", expand_tabs(line, tabstop)), ink))
                .collect()
        };
        let mut blocks = lines(base, '-', Ink::Removed);
        blocks.extend(lines(now, '+', Ink::Added));
        (!blocks.is_empty()).then(|| Self {
            widest: (blocks.iter()).map(Block::natural_width).max().unwrap_or(0),
            blocks,
            lines: Vec::new(),
            wrapped_to: 0,
            scroll: 0,
            placed: None,
            anchor,
            hovered: None,
        })
    }

    /// A list, as `:registers` shows, a line for each of `rows`, under a
    /// quieter `heading`. `None` if there are no rows.
    pub(crate) fn listing(
        heading: &str,
        rows: Vec<String>,
        anchor: crate::motion::Point,
    ) -> Option<Self> {
        if rows.is_empty() {
            return None;
        }
        let mut blocks = vec![code(heading.to_owned(), Ink::Muted)];
        blocks.extend(rows.into_iter().map(|row| code(row, Ink::Plain)));
        Self::of(blocks, anchor)
    }

    /// A completion item's documentation, as shown beside the menu: what
    /// it is, `detail`, as code, then `documentation`, as markdown if it
    /// says so. `None` if there's neither.
    pub(crate) fn documentation(
        detail: Option<&str>,
        documentation: Option<(&str, bool)>,
        tabstop: usize,
    ) -> Option<Self> {
        let mut blocks: Vec<Block> = (detail.into_iter())
            .flat_map(str::lines)
            .map(|line| code(expand_tabs(line, tabstop), Ink::Plain))
            .collect();
        if let Some((text, markdown)) = documentation {
            if !blocks.is_empty() {
                blocks.push(Block::Gap);
            }
            if markdown {
                blocks.extend(from_markdown(text, tabstop, &[]));
            } else {
                let lines = text.lines().map(|line| expand_tabs(line, tabstop));
                blocks.extend(lines.map(|line| Block::Prose(vec![plain(&line)])));
            }
        }
        Self::of(blocks, crate::motion::Point { line: 0, col: 0 })
    }

    /// A call's signature, `label`, with the argument being typed, at
    /// bytes `parameter` of it, in bold, and which of the function's
    /// `count` signatures it is, if it has several.
    pub(crate) fn signature(
        label: &str,
        parameter: Option<(usize, usize)>,
        (index, count): (usize, usize),
    ) -> Option<Self> {
        let parameter =
            parameter.filter(|&(start, end)| start <= end && label.get(start..end).is_some());
        let mut runs = match parameter {
            Some((start, end)) => vec![
                plain(&label[..start]),
                Run {
                    text: label[start..end].to_owned(),
                    ink: Ink::Bold,
                    code: false,
                },
                plain(&label[end..]),
            ],
            None => vec![plain(label)],
        };
        if count > 1 {
            runs.push(Run {
                text: format!("  {}/{count}", index + 1),
                ink: Ink::Muted,
                code: false,
            });
        }
        runs.retain(|run| !run.text.is_empty());
        let anchor = crate::motion::Point { line: 0, col: 0 };
        Self::of(vec![Block::Prose(runs)], anchor)
    }

    /// Wraps it for a pane `cols` wide, if that's not what it's wrapped
    /// for. Returns the width of its text.
    fn wrap(&mut self, cols: usize) -> usize {
        self.wrap_to(cols.saturating_sub(2).clamp(1, MAX_WIDTH))
    }

    /// Wraps it to `width` cells, or as narrow as its longest line, if
    /// that's narrower, unless that's what it's wrapped to. Returns the
    /// width of its text.
    fn wrap_to(&mut self, width: usize) -> usize {
        let width = width.max(1).min(self.widest.max(1));
        if width != self.wrapped_to {
            self.lines = (self.blocks.iter())
                .flat_map(|block| block.wrap(width))
                .collect();
            self.wrapped_to = width;
            self.scroll = self.scroll.min(self.lines.len().saturating_sub(1));
        }
        width
    }

    /// Works out where it goes in a pane `rows` by `cols` cells, the
    /// cursor being on row `line` and column `col` of it, before it's
    /// drawn. It isn't shown while the cursor's out of view.
    pub(crate) fn settle(&mut self, (rows, cols): (usize, usize), cursor: Option<(usize, usize)>) {
        let width = self.wrap(cols);
        let lines = self.lines.len();
        self.placed = cursor.and_then(|cursor| place((rows, cols), cursor, (width, lines)));
        if let Some(placed) = self.placed {
            self.scroll = self.scroll.min(self.lines.len() - placed.height);
        }
    }

    /// Works out where it goes beside `menu`, a completion menu, in a pane
    /// `rows` by `cols` cells: right of it, if there's room for `least`
    /// cells of text there, or else left of it, as wide as its text up to
    /// `most`. It stays on the menu's side of the cursor: level with its
    /// top if it's `below` the cursor, and as far down as the pane goes,
    /// or level with its bottom if it's above, and as far up. It isn't
    /// shown if there's room on neither side.
    pub(crate) fn settle_beside(
        &mut self,
        (rows, cols): (usize, usize),
        menu: Placed,
        below: bool,
        (least, most): (usize, usize),
    ) {
        self.placed = None;
        let menu_end = menu.left + menu.width + 2;
        let (right, left) = (cols.saturating_sub(menu_end), menu.left);
        let (room, on_right) = if right >= least + 2 {
            (right, true)
        } else if left >= least + 2 {
            (left, false)
        } else {
            return;
        };
        let width = self.wrap_to((room - 2).min(most));
        // The rows the box may take, a row more than its text's.
        let (from, to) = if below {
            (menu.top, rows)
        } else {
            (0, (menu.top + menu.height + 1).min(rows))
        };
        let height = (self.lines.len().min(MAX_HEIGHT)).min(to.saturating_sub(from + 1));
        if height == 0 {
            return;
        }
        self.placed = Some(Placed {
            top: if below { from } else { to - height - 1 },
            left: if on_right {
                menu_end
            } else {
                menu.left - width - 2
            },
            width,
            height,
        });
        self.scroll = self.scroll.min(self.lines.len() - height);
    }

    /// Works out where it goes by the cursor, at `cursor` of a pane `rows`
    /// by `cols` cells, before it's drawn: over the cursor's line, if
    /// `over` is free and there's room, or else under it, if `under` is.
    /// A few lines at most, as a call's signature takes. It isn't shown
    /// while the cursor's out of view.
    pub(crate) fn settle_over(
        &mut self,
        (rows, cols): (usize, usize),
        cursor: Option<(usize, usize)>,
        (over, under): (bool, bool),
    ) {
        /// The most lines shown.
        const MOST: usize = 4;
        self.placed = None;
        let Some((line, col)) = cursor else { return };
        if cols < 3 {
            return;
        }
        let width = self.wrap(cols);
        let wanted = self.lines.len().min(MOST) + 1;
        let top = if over && wanted <= line {
            line - wanted
        } else if under && wanted < rows.saturating_sub(line) {
            line + 1
        } else {
            return;
        };
        let boxed = (width + 2).min(cols);
        self.placed = Some(Placed {
            top,
            left: col.min(cols - boxed),
            width: boxed - 2,
            height: wanted - 1,
        });
        self.scroll = 0;
    }

    /// What it shows, and where, as last settled. `None` if the pane has
    /// no room for it.
    pub(crate) fn view(&self) -> Option<PanelView<'_>> {
        let placed = self.placed?;
        Some(PanelView {
            placed,
            lines: &self.lines[self.scroll..self.scroll + placed.height],
            first: self.scroll,
            total: self.lines.len(),
        })
    }

    /// Scrolls it by `lines`, down, or up if negative, as far as its last
    /// line coming into view.
    pub(crate) fn scroll(&mut self, lines: isize) {
        let height = self.placed.map_or(1, |placed| placed.height);
        let last = self.lines.len().saturating_sub(height);
        self.scroll = self.scroll.saturating_add_signed(lines).min(last);
    }

    /// Scrolls it down a page, keeping a line of the last, or back to the
    /// top from the end.
    pub(crate) fn page(&mut self) {
        let height = self.placed.map_or(1, |placed| placed.height);
        if self.scroll + height >= self.lines.len() {
            self.scroll = 0;
        } else {
            self.scroll += height.saturating_sub(1).max(1);
            self.scroll = self.scroll.min(self.lines.len() - height);
        }
    }
}

impl Block {
    /// How wide it is unwrapped, in cells.
    fn natural_width(&self) -> usize {
        match self {
            Self::Prose(runs) => {
                let text: String = runs.iter().map(|run| run.text.as_str()).collect();
                text.split('\n').map(mono_width).max().unwrap_or(0)
            }
            Self::Code(runs) => runs.iter().map(|run| mono_width(&run.text)).sum(),
            Self::Rule | Self::Gap => 0,
        }
    }

    /// Its lines, wrapped to `width` cells.
    fn wrap(&self, width: usize) -> Vec<Line> {
        match self {
            Self::Prose(runs) => wrap_prose(runs, width),
            Self::Code(runs) => {
                let chars: Vec<(char, usize)> = (runs.iter().enumerate())
                    .flat_map(|(index, run)| run.text.chars().map(move |ch| (ch, index)))
                    .collect();
                if chars.is_empty() {
                    return vec![Line {
                        runs: Vec::new(),
                        block: true,
                        picked: false,
                    }];
                }
                let mut lines = Vec::new();
                let mut rest = &chars[..];
                while !rest.is_empty() {
                    let (fits, _) = fit(rest, width);
                    let mut line = line_of(&rest[..fits], runs);
                    line.block = true;
                    lines.push(line);
                    rest = &rest[fits..];
                }
                lines
            }
            Self::Rule => vec![Line {
                runs: vec![Run {
                    text: "─".repeat(width),
                    ink: Ink::Muted,
                    code: false,
                }],
                block: false,
                picked: false,
            }],
            Self::Gap => vec![Line::default()],
        }
    }
}

/// Where a panel whose text is `width` by `lines` goes in a pane `rows`
/// by `cols`, the cursor being at row `line`, column `col`: under the
/// cursor if it fits there, or else over it if it fits there, or else
/// where there's more room, scrolled. Its left edge is the cursor's, as
/// far as the pane's right edge allows. `None` if there's no room for a
/// line of it either side.
pub(crate) fn place(
    (rows, cols): (usize, usize),
    (line, col): (usize, usize),
    (width, lines): (usize, usize),
) -> Option<Placed> {
    // The box is a row taller than its text, and two columns wider.
    let wanted = lines.min(MAX_HEIGHT) + 1;
    let below = rows.saturating_sub(line + 1);
    let above = line.min(rows);
    let (top, height) = if wanted <= below {
        (line + 1, wanted)
    } else if wanted <= above {
        (line - wanted, wanted)
    } else if below >= above {
        (line + 1, below)
    } else {
        (0, above)
    };
    if height < 2 || cols < 3 {
        return None;
    }
    let boxed = (width + 2).min(cols);
    Some(Placed {
        top,
        left: col.min(cols - boxed),
        width: boxed - 2,
        height: height - 1,
    })
}

/// Gaps at the end, as after a last paragraph, show nothing.
fn trim(blocks: &mut Vec<Block>) {
    while blocks.last() == Some(&Block::Gap) {
        blocks.pop();
    }
}

/// A line of code all in one ink.
fn code(text: String, ink: Ink) -> Block {
    Block::Code(vec![Run {
        text,
        ink,
        code: false,
    }])
}

/// `text`, which starts at byte `from` of the markdown it's from, if it's
/// the markdown's own text there, as runs coloured as `spans`, bytes of
/// the markdown in order, say. Text whose place isn't known is plain.
fn coloured(text: &str, from: Option<usize>, spans: &[Span]) -> Vec<Run> {
    let run = |text: &str, ink| Run {
        text: text.to_owned(),
        ink,
        code: false,
    };
    let Some(from) = from.filter(|_| !spans.is_empty()) else {
        return vec![run(text, Ink::Plain)];
    };
    let end = from + text.len();
    let mut runs = Vec::new();
    let mut done = from;
    for span in &spans[spans.partition_point(|span| span.end <= from)..] {
        if span.start >= end {
            break;
        }
        // One overlapping a span before, which a daemon never sends, is
        // coloured only past it.
        if span.end <= done {
            continue;
        }
        let (start, stop) = (span.start.max(done), span.end.min(end));
        // A span that doesn't fall on a character's edge is passed over.
        let (Some(before), Some(inside)) = (
            text.get(done - from..start - from),
            text.get(start - from..stop - from),
        ) else {
            continue;
        };
        if !before.is_empty() {
            runs.push(run(before, Ink::Plain));
        }
        runs.push(run(inside, colors::syntax_ink(span.kind)));
        done = stop;
    }
    if done < end {
        runs.push(run(&text[done - from..], Ink::Plain));
    }
    runs
}

/// `runs`, the runs of a line of code, as [`expand_tabs`] makes a line.
fn expand_tabs_in(mut runs: Vec<Run>, tabstop: usize) -> Vec<Run> {
    if !runs.iter().any(|run| run.text.contains('\t')) {
        return runs;
    }
    let tabstop = tabstop.max(1);
    let mut col = 0;
    for run in &mut runs {
        let mut expanded = String::with_capacity(run.text.len());
        for ch in run.text.chars() {
            if ch == '\t' {
                let blanks = tabstop - col % tabstop;
                expanded.extend(std::iter::repeat_n(' ', blanks));
                col += blanks;
            } else {
                expanded.push(ch);
                col += mono_cells(ch);
            }
        }
        run.text = expanded;
    }
    runs
}

/// `runs` of a block of code, split into its lines, without the line
/// breaks after the last: as the panel shows them, each line in runs of
/// one ink.
fn code_lines(runs: Vec<Run>) -> Vec<Vec<Run>> {
    let mut lines = vec![Vec::new()];
    for run in runs {
        let mut pieces = run.text.split('\n');
        if let Some(first) = pieces.next() {
            push_run(lines.last_mut().expect("never empty"), first, run.ink);
        }
        for piece in pieces {
            lines.push(Vec::new());
            push_run(lines.last_mut().expect("just pushed"), piece, run.ink);
        }
    }
    while lines.len() > 1 && lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    lines
}

/// Adds `text`, in `ink`, to the end of `runs`, joining the last run if
/// it's in the same ink.
fn push_run(runs: &mut Vec<Run>, text: &str, ink: Ink) {
    if text.is_empty() {
        return;
    }
    match runs.last_mut() {
        Some(last) if last.ink == ink => last.text.push_str(text),
        _ => runs.push(Run {
            text: text.to_owned(),
            ink,
            code: false,
        }),
    }
}

/// `line` with each tab made blanks to the next multiple of `tabstop`
/// columns, as a cell is a character in the panel.
fn expand_tabs(line: &str, tabstop: usize) -> String {
    if !line.contains('\t') {
        return line.to_owned();
    }
    let tabstop = tabstop.max(1);
    let mut expanded = String::with_capacity(line.len());
    let mut col = 0;
    for ch in line.chars() {
        if ch == '\t' {
            let blanks = tabstop - col % tabstop;
            expanded.extend(std::iter::repeat_n(' ', blanks));
            col += blanks;
        } else {
            expanded.push(ch);
            col += mono_cells(ch);
        }
    }
    expanded
}

/// Plain text, as a run.
fn plain(text: &str) -> Run {
    Run {
        text: text.to_owned(),
        ink: Ink::Plain,
        code: false,
    }
}

/// A diagnostic, in full: how much it matters, in its colour, what it
/// says, every line of it, and its name and who says so.
fn described(diagnostic: &Diagnostic) -> Vec<Block> {
    let severity = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
        Severity::Hint => "hint",
    };
    let mut lines = diagnostic.message.lines();
    let mut first = vec![
        Run {
            text: severity.to_owned(),
            ink: Ink::Color(colors::diagnostic(diagnostic.severity)),
            code: false,
        },
        plain(": "),
        plain(lines.next().unwrap_or_default()),
    ];
    let by: Vec<&str> = [&diagnostic.code, &diagnostic.source]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    let mut blocks = Vec::new();
    let rest: Vec<&str> = lines.collect();
    let named = (!by.is_empty()).then(|| Run {
        text: format!("[{}]", by.join(" ")),
        ink: Ink::Muted,
        code: false,
    });
    if rest.is_empty() {
        if let Some(mut named) = named {
            named.text.insert(0, ' ');
            first.push(named);
        }
        blocks.push(Block::Prose(first));
    } else {
        blocks.push(Block::Prose(first));
        for line in &rest {
            blocks.push(Block::Prose(vec![plain(line)]));
        }
        if let Some(named) = named {
            blocks.push(Block::Prose(vec![named]));
        }
    }
    blocks
}

/// Markdown, simply: headings and strong text bold, code in its box or
/// band, coloured as `spans`, bytes of `text` in order, say, lists with
/// their marks, links' text in the links' colour, and everything else as
/// plain text.
///
/// The daemon finds the code it colours with the same parser and options,
/// so its places are where this looks for them.
fn from_markdown(text: &str, tabstop: usize, spans: &[Span]) -> Vec<Block> {
    let mut reading = Markdown {
        tabstop,
        spans,
        blocks: Vec::new(),
        runs: Vec::new(),
        strong: 0,
        links: 0,
        lists: Vec::new(),
        code: None,
    };
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        // Where a piece of code is in the text, if it's the text's own.
        let from = match &event {
            Event::Text(piece) => {
                (text.get(range.clone()) == Some(&**piece)).then_some(range.start)
            }
            _ => None,
        };
        reading.take(event, from);
    }
    reading.end_paragraph();
    reading.blocks
}

/// Markdown being read into blocks.
struct Markdown<'a> {
    blocks: Vec<Block>,
    /// The paragraph or line being read.
    runs: Vec<Run>,
    /// How deep in strong text and headings, and in links.
    strong: usize,
    links: usize,
    /// The lists it's in, innermost last: each one's next number, or
    /// `None` for one with bullets.
    lists: Vec<Option<u64>>,
    /// The text of a block of code being read, in runs of its colours.
    code: Option<Vec<Run>>,
    /// The columns tabs in code stop at multiples of.
    tabstop: usize,
    /// How its code is coloured, as bytes of the markdown.
    spans: &'a [Span],
}

impl Markdown<'_> {
    /// Takes in `event`, whose text, if it has any, starts at byte `from`
    /// of the markdown, if it's the markdown's own.
    fn take(&mut self, event: Event<'_>, from: Option<usize>) {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                self.end_paragraph();
                self.strong += 1;
            }
            Event::End(TagEnd::Heading(_)) => {
                self.strong = self.strong.saturating_sub(1);
                self.end_paragraph();
            }
            Event::Start(Tag::Strong) => self.strong += 1,
            Event::End(TagEnd::Strong) => self.strong = self.strong.saturating_sub(1),
            Event::Start(Tag::Link { .. }) => self.links += 1,
            Event::End(TagEnd::Link) => self.links = self.links.saturating_sub(1),
            Event::Start(Tag::List(first)) => {
                self.end_paragraph();
                self.lists.push(first);
            }
            Event::End(TagEnd::List(_)) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blocks.push(Block::Gap);
                }
            }
            Event::Start(Tag::Item) => {
                self.end_line();
                let depth = self.lists.len().saturating_sub(1);
                let mark = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        *number += 1;
                        format!("{}. ", *number - 1)
                    }
                    _ => "• ".to_owned(),
                };
                self.runs
                    .push(plain(&format!("{}{mark}", "  ".repeat(depth))));
            }
            Event::End(TagEnd::Item | TagEnd::TableRow | TagEnd::TableHead) => self.end_line(),
            Event::End(TagEnd::Paragraph) if self.lists.is_empty() => self.end_paragraph(),
            Event::Start(Tag::CodeBlock(_)) => {
                self.end_paragraph();
                self.code = Some(Vec::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                let runs = self.code.take().unwrap_or_default();
                let tabstop = self.tabstop;
                let lines = code_lines(runs).into_iter();
                self.blocks
                    .extend(lines.map(|line| Block::Code(expand_tabs_in(line, tabstop))));
                self.blocks.push(Block::Gap);
            }
            Event::Text(text) => match &mut self.code {
                Some(code) => code.extend(coloured(&text, from, self.spans)),
                None => self.push(text.into_string(), false),
            },
            Event::Code(text) => self.push(text.into_string(), true),
            Event::SoftBreak => self.runs.push(plain(" ")),
            Event::HardBreak => self.runs.push(plain("\n")),
            Event::Rule => {
                self.end_paragraph();
                self.blocks.push(Block::Rule);
                self.blocks.push(Block::Gap);
            }
            Event::End(TagEnd::TableCell) => self.runs.push(plain("  ")),
            Event::InlineHtml(text) | Event::Html(text) => self.runs.push(plain(&text)),
            _ => {}
        }
    }

    /// Adds `text`, in the ink of where it is, as code within prose if
    /// `code`.
    fn push(&mut self, text: String, code: bool) {
        let ink = if self.links > 0 {
            Ink::Link
        } else if self.strong > 0 {
            Ink::Bold
        } else {
            Ink::Plain
        };
        self.runs.push(Run { text, ink, code });
    }

    /// Ends the line being read, as a list's item ends.
    fn end_line(&mut self) {
        if !self.runs.is_empty() {
            self.blocks
                .push(Block::Prose(std::mem::take(&mut self.runs)));
        }
    }

    /// Ends the paragraph being read, with a gap after it.
    fn end_paragraph(&mut self) {
        if !self.runs.is_empty() {
            self.end_line();
            self.blocks.push(Block::Gap);
        }
    }
}

/// `runs` wrapped to `width` cells, between words where it can be, and
/// wherever it reaches the edge where it can't. A `\n` breaks the line.
fn wrap_prose(runs: &[Run], width: usize) -> Vec<Line> {
    // Each character, with the run it's from.
    let cells: Vec<(char, usize)> = (runs.iter().enumerate())
        .flat_map(|(index, run)| run.text.chars().map(move |ch| (ch, index)))
        .collect();
    let mut lines = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        // A line's first blanks, after a wrap, aren't shown.
        if !lines.is_empty() && cells[start].0 == ' ' {
            start += 1;
            continue;
        }
        let rest = &cells[start..];
        let end = match fit(rest, width) {
            (fits, true) => {
                // The last blank that fits, or the first that doesn't, or
                // else the edge.
                let blank = (rest[..=fits].iter())
                    .rposition(|&(ch, _)| ch == ' ')
                    .filter(|&at| at > 0);
                start + blank.unwrap_or(fits)
            }
            (fits, false) => start + fits,
        };
        lines.push(line_of(&cells[start..end], runs));
        start = if cells.get(end).is_some_and(|&(ch, _)| ch == '\n') {
            end + 1
        } else {
            end
        };
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

/// How many of `cells`, from the start, fit in a line `width` cells wide:
/// up to a `\n`, which ends the line, or as many as fit, but at least
/// one. And whether more were left that didn't fit.
fn fit(cells: &[(char, usize)], width: usize) -> (usize, bool) {
    let mut used = 0;
    for (index, &(ch, _)) in cells.iter().enumerate() {
        if ch == '\n' {
            return (index, false);
        }
        let cells = mono_cells(ch);
        if used + cells > width && index > 0 {
            return (index, true);
        }
        used += cells;
    }
    (cells.len(), false)
}

/// `cells`, from `runs`, as a line of runs, each of one run's characters.
fn line_of(cells: &[(char, usize)], runs: &[Run]) -> Line {
    let mut line = Line::default();
    let mut last = None;
    for &(ch, index) in cells {
        if last != Some(index) {
            line.runs.push(Run {
                text: String::new(),
                ink: runs[index].ink,
                code: runs[index].code,
            });
            last = Some(index);
        }
        line.runs.last_mut().expect("just pushed").text.push(ch);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::Point;
    use clackatoo_text::Kind;

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::text).collect()
    }

    const HERE: Point = Point { line: 0, col: 0 };

    /// What the language server said, `text`, as markdown if `markdown`,
    /// answering query 1, about nothing it says.
    fn said(text: &str, markdown: bool) -> Hover<'_> {
        Hover {
            text,
            markdown,
            query: 1,
            range: None,
        }
    }

    /// `panel` as shown at the top of a pane 40 rows by 100 columns.
    fn shown(panel: &mut Panel) -> PanelView<'_> {
        panel.settle((40, 100), Some((0, 0)));
        panel.view().unwrap()
    }

    #[test]
    fn prose_wraps_between_words_and_code_at_the_edge() {
        let runs = [
            plain("one two "),
            Run {
                text: "three".into(),
                ink: Ink::Bold,
                code: false,
            },
            plain(" four"),
        ];
        let lines = wrap_prose(&runs, 9);
        assert_eq!(texts(&lines), ["one two", "three", "four"]);
        assert_eq!(lines[1].runs[0].ink, Ink::Bold);
        // A word longer than the line is cut at the edge.
        assert_eq!(
            texts(&wrap_prose(&[plain("abcdefghij k")], 4)),
            ["abcd", "efgh", "ij k"]
        );
        // Wide characters take two cells.
        assert_eq!(
            texts(&wrap_prose(&[plain("日本語 ok")], 5)),
            ["日本", "語 ok"]
        );
        // A hard break breaks.
        assert_eq!(texts(&wrap_prose(&[plain("a\nb")], 9)), ["a", "b"]);
        let code = super::code("fn main() {}".into(), Ink::Plain).wrap(5);
        assert_eq!(texts(&code), ["fn ma", "in() ", "{}"]);
        assert_eq!(
            texts(&super::code("日本語".into(), Ink::Plain).wrap(5)),
            ["日本", "語"]
        );
        assert!(code.iter().all(|line| line.block));
    }

    #[test]
    fn markdown_is_shown_simply() {
        let markdown =
            "# bad\n\nIt's **bad**: see `good`.\n\n```fake\nfn bad()\n```\n\n- one\n- two";
        let mut panel = Panel::new(&[], Some(said(markdown, true)), HERE, 4).unwrap();
        let view = shown(&mut panel);
        assert_eq!(
            texts(view.lines),
            [
                "bad",
                "",
                "It's bad: see good.",
                "",
                "fn bad()",
                "",
                "• one",
                "• two"
            ]
        );
        let heading = &view.lines[0].runs[0];
        assert_eq!(heading.ink, Ink::Bold);
        let prose = &view.lines[2].runs;
        assert!(
            prose
                .iter()
                .any(|run| run.text == "bad" && run.ink == Ink::Bold)
        );
        assert!(prose.iter().any(|run| run.text == "good" && run.code));
        assert!(view.lines[4].block);
        // Tabs in code go to the next stop, as gopls indents with them.
        let code = "```go\nfunc f() {\n\tx\ta\n}\n```";
        let mut panel = Panel::new(&[], Some(said(code, true)), HERE, 4).unwrap();
        assert_eq!(shown(&mut panel).lines[1].text(), "    x   a");
        // Plain text keeps its lines, and isn't read as markdown.
        let mut panel = Panel::new(&[], Some(said("# not\n**this**", false)), HERE, 4).unwrap();
        assert_eq!(texts(shown(&mut panel).lines), ["# not", "**this**"]);
    }

    #[test]
    fn code_is_coloured_once_its_colours_come_keeping_its_place() {
        // Lines enough to scroll, then a block with a tab, and one in a
        // list, indented, whose lines are apart in the markdown.
        let lines = (0..30)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n\n")
            + "\n\n";
        let markdown = format!("{lines}```go\nif\tx\n```\n\n- a\n\n  ```\n  y if\n  ```\n");
        let diagnostic = Diagnostic {
            start: 0,
            end: 1,
            severity: Severity::Warning,
            message: "unused".into(),
            source: None,
            code: None,
        };
        let hover = Hover {
            text: &markdown,
            markdown: true,
            query: 7,
            range: None,
        };
        let mut panel = Panel::new(&[&diagnostic], Some(hover), HERE, 4).unwrap();
        let plain = texts(shown(&mut panel).lines);
        panel.scroll(5);
        let keyword = |start: usize| Span {
            start,
            end: start + 2,
            kind: Kind::Keyword,
        };
        let first = markdown.find("if\t").unwrap();
        let second = markdown.find("y if").unwrap() + 2;
        let spans = [keyword(first), keyword(second)];
        // Colours for another query are someone else's.
        assert!(!panel.colour(8, &spans));
        assert!(panel.colour(7, &spans));
        let view = shown(&mut panel);
        assert_eq!(view.first, 5, "scrolled as it was");
        panel.scroll(100);
        let view = shown(&mut panel);
        // The same text, the diagnostic still above it, the code coloured.
        let lines = &view.lines[view.lines.len() - 5..];
        assert_eq!(texts(lines), ["if  x", "", "• a", "", "y if"]);
        let inks = |line: &Line| -> Vec<(String, Ink)> {
            (line.runs.iter())
                .map(|run| (run.text.clone(), run.ink))
                .collect()
        };
        let keyword = Ink::Color(colors::syntax(Kind::Keyword).fg);
        assert_eq!(
            inks(&lines[0]),
            [("if".to_owned(), keyword), ("  x".to_owned(), Ink::Plain)]
        );
        assert_eq!(
            inks(&lines[4]),
            [("y ".to_owned(), Ink::Plain), ("if".to_owned(), keyword)]
        );
        let mut all = plain;
        all.truncate(1);
        assert_eq!(all, ["warning: unused"]);
    }

    #[test]
    fn spans_that_overlap_or_split_a_character_are_coloured_only_where_they_can_be() {
        let span = |start, end, kind| Span { start, end, kind };
        let runs = |spans: &[Span]| -> Vec<(String, Ink)> {
            // "fn é", from byte 10 of the markdown: é is bytes 13 and 14.
            (coloured("fn é", Some(10), spans).into_iter())
                .map(|run| (run.text, run.ink))
                .collect()
        };
        let keyword = ("fn".to_owned(), colors::syntax_ink(Kind::Keyword));
        // One within the one before is passed over, as is one ending
        // inside a character.
        assert_eq!(
            runs(&[
                span(10, 12, Kind::Keyword),
                span(11, 12, Kind::String),
                span(12, 14, Kind::String),
            ]),
            [keyword.clone(), (" é".to_owned(), Ink::Plain)]
        );
        // One overlapping the one before is coloured past it.
        assert_eq!(
            runs(&[span(10, 12, Kind::Keyword), span(10, 15, Kind::String)]),
            [keyword, (" é".to_owned(), colors::syntax_ink(Kind::String))]
        );
    }

    #[test]
    fn a_diagnostic_is_shown_in_full_above_what_the_server_says() {
        let diagnostic = Diagnostic {
            start: 0,
            end: 1,
            severity: Severity::Error,
            message: "mismatched types\nexpected u32".into(),
            source: Some("rustc".into()),
            code: Some("E0308".into()),
        };
        let mut panel = Panel::new(&[&diagnostic], Some(said("u32", false)), HERE, 4).unwrap();
        let view = shown(&mut panel);
        assert_eq!(
            texts(view.lines),
            [
                "error: mismatched types".to_owned(),
                "expected u32".to_owned(),
                "[E0308 rustc]".to_owned(),
                // As wide as the widest line.
                "─".repeat("error: mismatched types".len()),
                "u32".to_owned(),
            ]
        );
        assert_eq!(
            view.lines[0].runs[0].ink,
            Ink::Color(colors::diagnostic(Severity::Error))
        );
        assert!(Panel::new(&[], None, HERE, 4).is_none());
    }

    #[test]
    fn the_panel_goes_under_the_cursor_or_over_it_whichever_fits() {
        // Under, its left edge the cursor's.
        assert_eq!(
            place((20, 80), (2, 10), (30, 5)),
            Some(Placed {
                top: 3,
                left: 10,
                width: 30,
                height: 5
            })
        );
        // No room under: over.
        assert_eq!(
            place((20, 80), (17, 10), (30, 5)),
            Some(Placed {
                top: 11,
                left: 10,
                width: 30,
                height: 5
            })
        );
        // Room for neither: where there's more, scrolled; and pushed in
        // from the right edge.
        assert_eq!(
            place((10, 40), (3, 30), (20, 50)),
            Some(Placed {
                top: 4,
                left: 18,
                width: 20,
                height: 5
            })
        );
        // Narrower than the pane: as wide as it can be.
        assert_eq!(place((10, 12), (0, 5), (30, 2)).unwrap().width, 10);
        // No room at all.
        assert_eq!(place((2, 80), (0, 0), (10, 1)), None);
    }

    #[test]
    fn a_long_panel_scrolls_and_pages() {
        let text: Vec<String> = (0..50).map(|n| n.to_string()).collect();
        let mut panel = Panel::new(&[], Some(said(&text.join("\n"), false)), HERE, 4).unwrap();
        let view = shown(&mut panel);
        assert_eq!(
            (view.lines.len(), view.first, view.total),
            (MAX_HEIGHT, 0, 50)
        );
        panel.scroll(3);
        assert_eq!(shown(&mut panel).lines[0].text(), "3");
        panel.scroll(-10);
        assert_eq!(shown(&mut panel).first, 0);
        panel.page();
        assert_eq!(shown(&mut panel).first, MAX_HEIGHT - 1);
        // Past the end, scrolling stops where the last line shows.
        panel.scroll(100);
        assert_eq!(shown(&mut panel).first, 50 - MAX_HEIGHT);
        // Paging from the end goes back to the top.
        panel.page();
        assert_eq!(shown(&mut panel).first, 0);
    }

    #[test]
    fn documentation_beside_the_menu_stays_on_its_side_of_the_cursor() {
        let text: Vec<String> = (0..30).map(|n| n.to_string()).collect();
        let mut docs = Panel::documentation(None, Some((&text.join("\n"), false)), 4).unwrap();
        // A menu under the cursor's row, 4: level with its top, as far
        // down as the pane goes, never up over the cursor.
        let under = Placed {
            top: 5,
            left: 0,
            width: 20,
            height: 3,
        };
        docs.settle_beside((12, 80), under, true, (10, 40));
        let placed = docs.view().unwrap().placed;
        assert_eq!((placed.top, placed.height, placed.left), (5, 6, 22));
        // A menu over it, rows 0 to 4: level with its bottom.
        let over = Placed { top: 0, ..under };
        docs.settle_beside((12, 80), over, false, (10, 40));
        let placed = docs.view().unwrap().placed;
        assert_eq!((placed.top, placed.height), (0, 3));
        // No room either side.
        docs.settle_beside((12, 30), under, true, (10, 40));
        assert!(docs.view().is_none());
    }
}
