//! Laying out and drawing tools as cards: a head saying what the tool
//! does and how it went, and under it, what it shows, folded to its first
//! lines, or for a command's output, its last, until it's clicked open.

use clackatoo_protocol::{Tool, ToolStatus};

use super::{Canvas, Laid, Look, Measures, draw_laid, lay_out};
use crate::chat::card::{Card, FileChange, Sign};
use crate::chat::{ChatView, Hit, Tone};
use crate::fonts::Fonts;
use crate::render::{Fill, Paint};

/// How many lines of what a card shows it shows folded.
const FOLDED: usize = 8;
/// The most lines it shows open.
const MOST: usize = 1000;
/// The space round a card's text, in ems.
const PAD: f32 = 0.8;

/// A card, laid out.
pub(super) struct CardLaid {
    /// The tool it was laid out from, and whether it was open: when
    /// either changes, it's laid out again.
    tool: Tool,
    open: bool,
    /// How the tool went: "✓".
    mark: Laid,
    /// What it does: a command, or a change's title.
    head: Laid,
    /// How many lines a change adds and removes.
    counts: Option<(Laid, Laid)>,
    /// What goes before a change's lines.
    gutter: Option<Gutter>,
    body: Vec<Section>,
    /// What clicking it does, if it has more to show, or less.
    footer: Option<Laid>,
    head_height: f32,
    pub(super) height: f32,
}

/// A piece of what a card shows.
enum Section {
    /// Text across the card: a command's output, or what it's for.
    Text(Laid),
    /// A file's path, over its lines, when a change is to several.
    Path(Laid),
    /// A line of a change: its sign, its number and its text.
    Line(Sign, Option<Laid>, Laid),
    /// How many lines are left out.
    More(Laid),
}

/// What goes before a change's lines: their numbers, if they're known,
/// and the signs of those added and removed.
struct Gutter {
    added: Laid,
    removed: Laid,
    /// How wide the widest number is, or 0 with none.
    numbers: f32,
}

impl Gutter {
    /// How far the lines are from the left.
    fn width(&self, m: Measures) -> f32 {
        let numbers = if self.numbers > 0.0 {
            self.numbers + m.em(0.8)
        } else {
            0.0
        };
        numbers + self.added.width + m.em(0.6)
    }
}

impl Section {
    fn height(&self, m: Measures) -> f32 {
        match self {
            Self::Path(laid) => laid.height + m.em(0.3),
            Self::Text(laid) | Self::Line(_, _, laid) | Self::More(laid) => laid.height,
        }
    }
}

impl ChatView {
    /// The card for `tool`, piece `key` of the conversation, laid out
    /// `width` wide, from the cache if neither it nor whether it's open
    /// changed. Returns its height.
    pub(super) fn card(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        key: (usize, usize),
        tool: &Tool,
        width: f32,
    ) -> f32 {
        let open = self.open_cards.contains(&tool.id);
        if let Some(laid) = self.cache.cards.get(&key)
            && laid.open == open
            && laid.tool == *tool
        {
            return laid.height;
        }
        let laid = self.lay_out_card(fonts, m, tool, open, width);
        let height = laid.height;
        self.cache.cards.insert(key, laid);
        height
    }

    fn lay_out_card(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        tool: &Tool,
        open: bool,
        width: f32,
    ) -> CardLaid {
        let card = Card::of(tool);
        let inner = (width - 2.0 * m.em(PAD)).max(m.em(4.0));
        let mut w = Writer {
            layout_cx: &mut self.layout_cx,
            fonts,
            m,
            inner,
            open,
        };
        let (mark, tone) = match tool.status {
            ToolStatus::Running => ("○", Tone::Muted),
            ToolStatus::Done => ("✓", Tone::Muted),
            ToolStatus::Failed => ("✗", Tone::Warning),
        };
        let mark = w.text(mark, Look::Plain(tone), inner);
        let counts = (card.as_ref().and_then(Card::counts)).map(|(added, removed)| {
            (
                w.text(&format!("+{added}"), Look::Plain(Tone::Added), inner),
                w.text(&format!("−{removed}"), Look::Plain(Tone::Removed), inner),
            )
        });
        // The head leaves room for the mark before it and the counts after.
        let head_wide = inner
            - mark.width
            - m.em(0.6)
            - (counts.as_ref()).map_or(0.0, |(added, removed)| {
                added.width + removed.width + m.em(1.5)
            });
        let head = match &card {
            Some(Card::Command { command, .. }) => {
                w.text(&format!("$ {command}"), Look::Mono(Tone::Plain), head_wide)
            }
            _ => w.text(&tool.title, Look::Plain(Tone::Plain), head_wide),
        };
        let (body, gutter, total) = match &card {
            Some(Card::Command {
                purpose, output, ..
            }) => {
                let (body, total) = w.command(purpose.as_deref(), output);
                (body, None, total)
            }
            Some(Card::Change(files)) => w.change(files),
            None => (Vec::new(), None, 0),
        };
        let footer = (total > FOLDED).then(|| {
            let says = if open {
                "Show less".to_owned()
            } else {
                format!("Show all {total} lines")
            };
            w.text(&says, Look::Plain(Tone::Muted), inner)
        });
        let head_height = mark.height.max(head.height) + 2.0 * m.em(0.55);
        let mut height = head_height;
        if !body.is_empty() {
            height +=
                1.0 + 2.0 * m.em(0.45) + body.iter().map(|section| section.height(m)).sum::<f32>();
        }
        if footer.is_some() {
            height += 1.0 + m.em(1.8);
        }
        CardLaid {
            tool: tool.clone(),
            open,
            mark,
            head,
            counts,
            gutter,
            body,
            footer,
            head_height,
            height,
        }
    }
}

/// Lays out a card's text.
struct Writer<'a> {
    layout_cx: &'a mut parley::LayoutContext<crate::chat::Brush>,
    fonts: &'a mut Fonts,
    m: Measures,
    /// How wide the card's text is.
    inner: f32,
    /// Whether the card is open.
    open: bool,
}

impl Writer<'_> {
    fn text(&mut self, text: &str, look: Look, wide: f32) -> Laid {
        lay_out(self.layout_cx, self.fonts, self.m, text, look, wide)
    }

    /// How wide the lines of what it shows are: folded, a line is a line,
    /// cut off at the card's edge, and open, it wraps, to be read whole.
    fn body_wide(&self) -> f32 {
        if self.open { self.inner } else { f32::INFINITY }
    }

    /// What a command's card shows, and how many lines it has: the end of
    /// its output, which is where what went wrong is, or until it has
    /// any, what it's for.
    fn command(&mut self, purpose: Option<&str>, output: &[String]) -> (Vec<Section>, usize) {
        let mut body = Vec::new();
        let total = output.len();
        if output.is_empty() {
            if let Some(purpose) = purpose {
                body.push(Section::Text(self.text(
                    purpose,
                    Look::Plain(Tone::Muted),
                    self.inner,
                )));
            }
            return (body, total);
        }
        let shown = total.min(if self.open { MOST } else { FOLDED });
        if shown < total && self.open {
            let left_out = format!("{} earlier lines not shown", total - shown);
            body.push(Section::More(self.text(
                &left_out,
                Look::Plain(Tone::Muted),
                self.inner,
            )));
        }
        let lines = output[total - shown..].join("\n");
        let wide = self.body_wide();
        body.push(Section::Text(self.text(
            &lines,
            Look::Mono(Tone::Plain),
            wide,
        )));
        (body, total)
    }

    /// What a change's card shows, what goes before its lines, and how
    /// many lines it has: from the start, each file's lines, under its
    /// path if there are several.
    fn change(&mut self, files: &[FileChange]) -> (Vec<Section>, Option<Gutter>, usize) {
        let total = files.iter().map(|file| file.lines.len()).sum();
        let most = if self.open { MOST } else { FOLDED };
        let shown = || files.iter().flat_map(|file| &file.lines).take(most);
        let widest = shown().filter_map(|line| line.number).max();
        let numbers = widest.map_or(0.0, |widest| {
            self.text(&widest.to_string(), Look::Mono(Tone::Muted), f32::INFINITY)
                .width
        });
        let gutter = Gutter {
            added: self.text("+", Look::Mono(Tone::Added), self.inner),
            removed: self.text("−", Look::Mono(Tone::Removed), self.inner),
            numbers,
        };
        // Lines wrap after the gutter.
        let lines_wide = self.body_wide() - gutter.width(self.m);
        let mut body = Vec::new();
        let mut left = most;
        for file in files {
            if left == 0 {
                break;
            }
            if files.len() > 1 {
                body.push(Section::Path(self.text(
                    &file.path,
                    Look::Mono(Tone::Muted),
                    self.inner,
                )));
            }
            for line in file.lines.iter().take(left) {
                let tone = if line.sign == Sign::Kept {
                    Tone::Muted
                } else {
                    Tone::Plain
                };
                let number = (line.number).map(|number| {
                    self.text(&number.to_string(), Look::Mono(Tone::Muted), f32::INFINITY)
                });
                // An empty line still takes a line.
                let text = if line.text.is_empty() {
                    " "
                } else {
                    &line.text
                };
                let text = self.text(text, Look::Mono(tone), lines_wide);
                body.push(Section::Line(line.sign, number, text));
            }
            left = left.saturating_sub(file.lines.len());
        }
        if self.open && total > MOST {
            let left_out = format!("{} more lines not shown", total - MOST);
            body.push(Section::More(self.text(
                &left_out,
                Look::Plain(Tone::Muted),
                self.inner,
            )));
        }
        (body, Some(gutter), total)
    }
}

/// Draws `card` with its top left at `origin`, `width` wide, and if it
/// folds, says where it can be clicked: `hits` gets the part of it in
/// view, from `pane`, the pane's top left.
pub(super) fn draw_card(
    canvas: &mut Canvas<'_>,
    m: Measures,
    card: &CardLaid,
    origin: [f32; 2],
    width: f32,
    (pane, hits): ([f32; 2], &mut Vec<(Hit, [f32; 4])>),
) {
    let rect = [origin[0], origin[1], width, card.height];
    canvas.fill(Fill::rounded(rect, Paint::Code, m.em(0.6)).bordered(1.0, Paint::RaisedEdge));
    // What's drawn in it stays in it, and in view.
    let view = canvas.clip;
    canvas.clip = intersection(view, [rect[0] + 1.0, rect[1], rect[2] - 2.0, rect[3]]);
    let inner = width - 2.0 * m.em(PAD);
    let x = origin[0] + m.em(PAD);
    let divider = |canvas: &mut Canvas<'_>, y: f32| {
        canvas.fill(Fill::solid(
            [origin[0] + 1.0, y, width - 2.0, 1.0],
            Paint::RaisedEdge,
        ));
    };

    // The head: how it went, what it does, and what a change adds and
    // removes, on the right.
    let middle = |laid: &Laid| origin[1] + (card.head_height - laid.height) / 2.0;
    draw_laid(canvas, m, &card.mark, [x, middle(&card.mark)], inner);
    let head_x = x + card.mark.width + m.em(0.6);
    draw_laid(canvas, m, &card.head, [head_x, middle(&card.head)], inner);
    if let Some((added, removed)) = &card.counts {
        let right = origin[0] + width - m.em(PAD);
        let removed_x = right - removed.width;
        let added_x = removed_x - m.em(0.5) - added.width;
        draw_laid(canvas, m, added, [added_x, middle(added)], inner);
        draw_laid(canvas, m, removed, [removed_x, middle(removed)], inner);
    }
    let mut y = origin[1] + card.head_height;

    if !card.body.is_empty() {
        divider(canvas, y);
        y += 1.0 + m.em(0.45);
        for section in &card.body {
            match section {
                Section::Text(laid) | Section::More(laid) => {
                    draw_laid(canvas, m, laid, [x, y], inner);
                }
                Section::Path(laid) => draw_laid(canvas, m, laid, [x, y + m.em(0.15)], inner),
                Section::Line(sign, number, laid) => {
                    draw_line(
                        canvas,
                        m,
                        card,
                        (*sign, number.as_ref(), laid),
                        [x, y],
                        width,
                    );
                }
            }
            y += section.height(m);
        }
        y += m.em(0.45);
    }

    if let Some(footer) = &card.footer {
        divider(canvas, y);
        let row = m.em(1.8);
        draw_laid(
            canvas,
            m,
            footer,
            [x, y + 1.0 + (row - footer.height) / 2.0],
            inner,
        );
        // Only what's in view takes a click: the rest is under the box.
        let [left, top, wide, high] = intersection(view, rect);
        if wide > 0.0 && high > 0.0 {
            hits.push((
                Hit::Card(card.tool.id.clone()),
                [left - pane[0], top - pane[1], wide, high],
            ));
        }
    }
    canvas.clip = view;
}

/// Where rectangles `a` and `b` overlap: nothing wide or high, if they
/// don't.
fn intersection(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let (left, top) = (a[0].max(b[0]), a[1].max(b[1]));
    let right = (a[0] + a[2]).min(b[0] + b[2]);
    let bottom = (a[1] + a[3]).min(b[1] + b[3]);
    [left, top, (right - left).max(0.0), (bottom - top).max(0.0)]
}

/// Draws a line of a change at `at`: on green or red if it's added or
/// removed, after its number and sign.
fn draw_line(
    canvas: &mut Canvas<'_>,
    m: Measures,
    card: &CardLaid,
    (sign, number, laid): (Sign, Option<&Laid>, &Laid),
    at: [f32; 2],
    width: f32,
) {
    let Some(gutter) = &card.gutter else {
        return;
    };
    let left = at[0] - m.em(PAD) + 1.0;
    let row = [left, at[1], width - 2.0, laid.height];
    let mark = match sign {
        Sign::Added => {
            canvas.fill(Fill::solid(row, Paint::Added));
            Some(&gutter.added)
        }
        Sign::Removed => {
            canvas.fill(Fill::solid(row, Paint::Removed));
            Some(&gutter.removed)
        }
        Sign::Kept => None,
    };
    // Numbers line up on the right.
    if let Some(number) = number {
        let x = at[0] + gutter.numbers - number.width;
        draw_laid(canvas, m, number, [x, at[1]], gutter.numbers);
    }
    let text_x = at[0] + gutter.width(m);
    if let Some(mark) = mark {
        let x = text_x - m.em(0.6) - mark.width;
        draw_laid(canvas, m, mark, [x, at[1]], mark.width);
    }
    draw_laid(canvas, m, laid, [text_x, at[1]], width);
}
