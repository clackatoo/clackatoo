//! Tools start as quiet single-line summaries. Opening a row reveals its
//! full title and output or diff; a failed tool keeps a short excerpt visible.

use clackatoo_protocol::{Tool, ToolStatus};

use super::select::TextKey;
use super::{Canvas, Drawn, Laid, Look, Measures, draw_laid, intersection, lay_out};
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
    chevron: Laid,
    /// How many lines a change adds and removes.
    counts: Option<(Laid, Laid)>,
    /// What goes before a change's lines.
    gutter: Option<Gutter>,
    body: Vec<Section>,
    /// Where each of `body` starts, from the card's top.
    tops: Vec<f32>,
    /// A second way to collapse a long open card without scrolling to its head.
    footer: Option<Laid>,
    head_height: f32,
    pub(super) height: f32,
}

impl CardLaid {
    /// Part `part` of its text, and where it's drawn from the card's top
    /// left: 0 is its head, and what it shows follows, a part to a line
    /// of a change, or to a command's output.
    pub(in crate::chat) fn part(&self, m: Measures, part: usize) -> Option<(&Laid, [f32; 2])> {
        if part == 0 {
            let x = m.em(PAD) + self.mark.width + m.em(0.6);
            return Some((&self.head, [x, (self.head_height - self.head.height) / 2.0]));
        }
        let (section, top) = (self.body.get(part - 1)?, self.tops.get(part - 1)?);
        let (laid, x, down) = match section {
            Section::Text(laid) | Section::More(laid) => (laid, m.em(PAD), 0.0),
            Section::Path(laid) => (laid, m.em(PAD), m.em(0.15)),
            Section::Line(_, _, laid) => {
                let gutter = self.gutter.as_ref().map_or(0.0, |gutter| gutter.width(m));
                (laid, m.em(PAD) + gutter, 0.0)
            }
        };
        Some((laid, [x, top + down]))
    }

    /// The first of `body` that reaches `y` from the card's top, or past
    /// it: what's in view starts there.
    fn first_reaching(&self, y: f32) -> usize {
        self.tops.partition_point(|&top| top < y).saturating_sub(1)
    }

    /// How many parts of text it has: its head and its sections.
    pub(in crate::chat) fn parts(&self) -> usize {
        1 + self.body.len()
    }
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
    /// The card for `tool`, laid out `width` wide: `old`, if neither the
    /// tool nor whether its card is open has changed since it was laid
    /// out.
    pub(super) fn card(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        old: Option<CardLaid>,
        tool: &Tool,
        width: f32,
    ) -> CardLaid {
        let open = self.open_cards.contains(&tool.id);
        match old {
            Some(laid) if laid.open == open && laid.tool == *tool => laid,
            _ => self.lay_out_card(fonts, m, tool, open, width),
        }
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
        let chevron = w.text(
            if open { "⌄" } else { "›" },
            Look::Plain(Tone::Muted),
            inner,
        );
        let head_wide = (head_wide - chevron.width - m.em(0.8)).max(m.em(2.0));
        let (title, look) = match &card {
            Some(Card::Command { command, .. }) => {
                (format!("$ {command}"), Look::Mono(Tone::Plain))
            }
            _ => (tool.title.clone(), Look::Plain(Tone::Muted)),
        };
        let head = if open {
            w.text(&title, look, head_wide)
        } else {
            w.summary(&title, head_wide)
        };
        let (body, gutter, _) = match &card {
            _ if !open && tool.status != ToolStatus::Failed => (Vec::new(), None, 0),
            Some(Card::Command {
                purpose, output, ..
            }) => {
                let (body, total) = w.command(purpose.as_deref(), output);
                (body, None, total)
            }
            Some(Card::Change(files)) => w.change(files),
            None => (Vec::new(), None, 0),
        };
        let footer =
            (open && !body.is_empty()).then(|| w.text("Collapse", Look::Plain(Tone::Muted), inner));
        let head_height = mark.height.max(head.height) + 2.0 * m.em(0.35);
        let mut top = head_height + 1.0 + m.em(0.45);
        let tops = (body.iter())
            .map(|section| {
                let this = top;
                top += section.height(m);
                this
            })
            .collect();
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
            chevron,
            counts,
            gutter,
            body,
            tops,
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
    /// A single readable line, with an ellipsis measured in the actual font.
    /// The complete title remains available when the row is opened.
    fn summary(&mut self, text: &str, wide: f32) -> Laid {
        let text = summary_title(text);
        let look = Look::Plain(Tone::Muted);
        let full = self.text(&text, look, f32::INFINITY);
        if full.width <= wide {
            return full;
        }
        let boundaries: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
        let (mut low, mut high) = (0, boundaries.len());
        let mut best = self.text("…", look, f32::INFINITY);
        while low < high {
            let mid = low + (high - low) / 2;
            let candidate = self.text(
                &format!("{}…", &text[..boundaries[mid]]),
                look,
                f32::INFINITY,
            );
            if candidate.width <= wide {
                best = candidate;
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        best
    }

    fn text(&mut self, text: &str, look: Look, wide: f32) -> Laid {
        lay_out(
            self.layout_cx,
            self.fonts,
            Measures::of(self.m.em * 0.9),
            text,
            look,
            wide,
        )
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

/// File tools often repeat the entire workspace path. A basename identifies
/// the file in the row; opening it shows the original, unabridged title.
fn summary_title(text: &str) -> String {
    for verb in ["Read ", "View Image ", "Edit ", "Write "] {
        if let Some(path) = text.strip_prefix(verb)
            && path.starts_with('/')
            && !path.contains(char::is_whitespace)
            && let Some(name) = path.rsplit('/').next().filter(|name| !name.is_empty())
        {
            return format!("{verb}{name}");
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Draws `card`, text `key`'s card, with its top left at `origin`,
/// `width` wide, noting its text in `drawn`, and if it folds, where it
/// can be clicked: its head and its foot, where they're in view. Its
/// text, between, is for selecting.
pub(super) fn draw_card(
    canvas: &mut Canvas<'_>,
    m: Measures,
    card: &CardLaid,
    key: TextKey,
    origin: [f32; 2],
    width: f32,
    drawn: &mut Drawn<'_>,
) {
    let rect = [origin[0], origin[1], width, card.height];
    if card.open || !card.body.is_empty() {
        canvas.fill(Fill::rounded(rect, Paint::Code, m.em(0.6)).bordered(1.0, Paint::RaisedEdge));
    }
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
    let right = origin[0] + width - m.em(PAD);
    draw_laid(
        canvas,
        m,
        &card.chevron,
        [right - card.chevron.width, middle(&card.chevron)],
        inner,
    );
    if let Some((added, removed)) = &card.counts {
        let right = right - card.chevron.width - m.em(0.8);
        let removed_x = right - removed.width;
        let added_x = removed_x - m.em(0.5) - added.width;
        draw_laid(canvas, m, added, [added_x, middle(added)], inner);
        draw_laid(canvas, m, removed, [removed_x, middle(removed)], inner);
    }
    let mut y = origin[1] + card.head_height;

    // Only what's in view, of what may be a thousand lines.
    let [view_top, view_bottom] = [view[1], view[1] + view[3]];
    let first = card.first_reaching(view_top - origin[1]);
    if !card.body.is_empty() {
        divider(canvas, y);
        for (section, &top) in card.body[first..].iter().zip(&card.tops[first..]) {
            let y = origin[1] + top;
            if y > view_bottom {
                break;
            }
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
        }
        y = origin[1] + card.tops.last().copied().unwrap_or(0.0);
        y += card.body.last().map_or(0.0, |section| section.height(m)) + m.em(0.45);
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
    }
    // Every tool row opens, including a short command or a long file path.
    let head = [rect[0], rect[1], rect[2], card.head_height];
    let foot = [rect[0], y, rect[2], rect[1] + rect[3] - y];
    for part in std::iter::once(head).chain(card.footer.as_ref().map(|_| foot)) {
        drawn.hit(view, Hit::Card(key.turn, card.tool.id.clone()), part);
    }
    canvas.clip = view;
    let parts = std::iter::once(0).chain(first + 1..card.parts());
    for part in parts {
        let Some((laid, [x, y])) = card.part(m, part) else {
            continue;
        };
        if origin[1] + y > view_bottom {
            break;
        }
        if let Some(block) = laid.blocks.first()
            && origin[1] + y + laid.height >= view_top
        {
            let at = [origin[0] + x + block.left, origin[1] + y + block.top];
            drawn.note_text(TextKey { part, ..key }, at, &block.layout, false);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::fixtures::{fonts, paint, read, read_named, talk};
    use crate::chat::paint::{ITEMS, Position};
    use clackatoo_protocol::{Item, ToolContent, ToolKind};
    use winit::keyboard::NamedKey;

    #[test]
    fn long_commands_fold_without_losing_output_or_keyboard_access() {
        let mut fonts = fonts();
        let mut node = talk(&[("Check this", "")]);
        let title = "cargo test --manifest-path /a/long/路径/Cargo.toml ".repeat(12);
        node.turns[0].items = vec![Item::Tool(Tool {
            id: "test".into(),
            title: title.clone(),
            status: ToolStatus::Done,
            kind: ToolKind::Execute,
            content: vec![ToolContent::Output("All tests passed".into())],
            locations: vec![],
        })];
        let mut chat = ChatView::new(String::new(), node);
        chat.open_fold(0);
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        let key = TextKey {
            turn: 0,
            piece: ITEMS,
            part: 0,
        };
        let card = &chat.cache.turns[0].laid.as_ref().unwrap().cards[&ITEMS];
        assert!(card.height < 40.0);
        assert!(card.head.source.ends_with('…'));
        assert!(card.body.is_empty());
        assert!(
            chat.hits
                .iter()
                .any(|(hit, _)| *hit == Hit::Card(0, "test".into()))
        );
        chat.cursor = Some(Position { text: key, byte: 0 });
        read_named(&mut chat, NamedKey::Enter);
        assert!(chat.reading);
        paint(&mut chat, &mut fonts);
        let card = &chat.cache.turns[0].laid.as_ref().unwrap().cards[&ITEMS];
        assert_eq!(card.head.source, format!("$ {title}"));
        assert!(matches!(&card.body[0], Section::Text(text) if text.source == "All tests passed"));
        read(&mut chat, "zc");
        paint(&mut chat, &mut fonts);
        assert!(
            chat.cache.turns[0].laid.as_ref().unwrap().cards[&ITEMS]
                .body
                .is_empty()
        );
        assert_eq!(chat.cursor.unwrap().text, key);
    }

    #[test]
    fn failed_commands_keep_the_end_of_their_output_visible() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), talk(&[]));
        let tool = Tool {
            id: "failed".into(),
            title: "cargo test".into(),
            status: ToolStatus::Failed,
            kind: ToolKind::Execute,
            content: vec![ToolContent::Output("Error: test failed".into())],
            locations: vec![],
        };
        let card = chat.card(&mut fonts, Measures::of(16.0), None, &tool, 500.0);
        assert!(!card.open);
        assert!(
            matches!(&card.body[0], Section::Text(text) if text.source == "Error: test failed")
        );
    }
}
