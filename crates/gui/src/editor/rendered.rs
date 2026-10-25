//! An editor's rendered view of a Markdown file: its text read as Markdown
//! and drawn as prose in place of the text, as a chat draws its agent's
//! words (see [`crate::markdown`]). `:markdown`, or `<Space>tm`, turns it
//! on and off, keeping the place: the view opens with the block holding
//! the line at the top of the text's view at its top, and the text comes
//! back with the line the top block starts on at the top.
//!
//! It's a view of the buffer, whoever changes it: an edit made in another
//! pane, by an agent or in another window shows as it's made. Nothing of
//! it is worked out while it isn't drawn. When it is, if the text has
//! changed since, it's read again, and only the blocks that changed are
//! laid out again, the rest kept by what they were laid out from, as a
//! chat keeps the paragraphs of an answer still coming. The block at the
//! top of the view stays there, however edits above it move it. Turned off,
//! the view is kept, so turning it on again lays out only what's changed.
//! The text is read as a whole each time it's changed, which takes about
//! half a millisecond for a README of a few thousand lines.
//!
//! It's for reading, as a pager: the focus arriving at it in insert mode
//! leaves it in normal mode. `j`, `k`, `gj`, `gk`, Ctrl+E and Ctrl+Y scroll
//! it a line, or as many as a count says; Ctrl+D and Ctrl+U half a view,
//! Ctrl+F and Ctrl+B nearly a whole one; `gg` to the top and `G` to the end,
//! or either with a count, to the block holding that line; and so does the
//! mouse's wheel. A count, or `g`, waits in the editor's keys, as one typed
//! in the text does. Any other key, such as `i`, `dd` or `/`, or a paste,
//! is the text's: the text comes back, with the cursor at the start of the
//! line the top block starts on, and the key does there what it does. The
//! command line's commands, as `:w` and `:q`, are the same in both.
//!
//! A click on a link opens it (see `desk::markdown`), and `#` and a
//! heading, as GitHub names headings, scrolls to that heading. An image is
//! its text, marked as an image. Its text can't be selected, nor searched:
//! that's done in the text.

use clackatoo_protocol::{Code, Span};
use clackatoo_text::Rope;
use parley::LayoutContext;

use super::{BufferCopy, Done, Editor, Shared};
use crate::fonts::Fonts;
use crate::markdown::colours::Colours;
use crate::markdown::layout::{
    Brush, Canvas, LINE, Laid, Look, Measures, TextPaint, draw_laid, lay_out_blocks, text_size,
};
use crate::markdown::tables::Cells;
use crate::markdown::{self, BlockKind};
use crate::vim::Mode;
use crate::vim::{self, Stroke};

/// The widest the column of text gets, in ems: as a chat's, about as wide
/// as a printed page's lines, and wide enough for 80 columns of code.
const COLUMN: f32 = 46.0;

/// A Markdown file, rendered.
pub(crate) struct Rendered {
    page: Option<Page>,
    layout_cx: LayoutContext<Brush>,
    /// The colours of its code, as the daemon read it.
    colours: Colours,
    /// Colours have come since it was laid out, which some code waited for.
    recoloured: bool,
    /// How far down the page the view is, in pixels: 0 shows its top.
    scroll: f32,
    /// The byte of the text whose block is to go to the top of the view
    /// once it's laid out.
    go_to: Option<usize>,
    /// How tall a line of its text is, the view, and the space above the
    /// page, and how far it can scroll, as drawn last: what keys scroll by.
    line_height: f32,
    view_height: f32,
    margin: f32,
    most: f32,
    /// How far its scrolling keys go.
    paging: vim::Paging,
    /// The blocks with links drawn last, each with where its text's top
    /// left was, from the pane's top left, and what of it was drawn, its
    /// left, top, width and height, as a table's cells are cut off at the
    /// column's edge, for clicks.
    shown: Vec<(usize, [f32; 2], [f32; 4])>,
    /// How many blocks it last laid out afresh, rather than kept.
    #[cfg(test)]
    pub(super) fresh: usize,
}

/// The text, laid out.
struct Page {
    /// The text's revision, and the column's width, the text's size and
    /// the fonts' generation, it was laid out for.
    revision: u64,
    width: f32,
    size: f32,
    fonts: u32,
    laid: Laid,
}

/// What a key typed in the view did.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Keyed {
    /// It scrolled the view, or waits for the rest of its command.
    Took,
    /// It isn't the view's: the text's, which comes back, to have the keys
    /// typed before it, a count or `g`, then it.
    Text(Vec<Stroke>),
}

impl std::fmt::Debug for Rendered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rendered")
            .field("scroll", &self.scroll)
            .field("go_to", &self.go_to)
            .finish_non_exhaustive()
    }
}

impl Rendered {
    /// A view of a text, opening with the block holding its byte `at` at
    /// the top.
    pub(super) fn new(at: usize) -> Self {
        Self {
            page: None,
            layout_cx: LayoutContext::new(),
            colours: Colours::default(),
            recoloured: false,
            scroll: 0.0,
            go_to: Some(at),
            // Until it's drawn, about what a pane's are.
            line_height: 20.0,
            view_height: 600.0,
            margin: 0.0,
            most: f32::INFINITY,
            paging: vim::Paging::default(),
            shown: Vec::new(),
            #[cfg(test)]
            fresh: 0,
        }
    }

    /// Shows the view again, as it was put away, with the block holding
    /// byte `at` of the text at the top.
    pub(super) fn open_at(&mut self, at: usize) {
        self.go_to = Some(at);
    }

    /// Does what `stroke` does, if it's one of the view's keys, in a text
    /// that's `text`, after `typed`, the keys of a command typed before it:
    /// a count, or `g`. The editor keeps them, as it keeps its own, so its
    /// bar shows them, and leaving the pane forgets them.
    pub(super) fn key(&mut self, stroke: Stroke, typed: &mut vim::Keys, text: &Rope) -> Keyed {
        let g = Stroke::char('g');
        let after_g = typed.so_far().last() == Some(&g);
        let count: Option<usize> = (typed.so_far().iter())
            .filter_map(|&typed| digit(typed))
            .collect::<String>()
            .parse()
            .ok();
        // `0` goes on a count, but doesn't start one.
        let counting = digit(stroke).is_some_and(|digit| digit != '0' || count.is_some());
        if !after_g && (counting || stroke == g) {
            typed.push(stroke);
            return Keyed::Took;
        }
        let before = typed.take();
        // `gj` and `gk` go by screen rows, which here are lines, as `j`
        // and `k` do: a config may map `j` to `gj`.
        let rows = after_g
            && matches!(
                stroke,
                Stroke::Char {
                    ch: 'j' | 'k',
                    ctrl: false
                }
            );
        let line =
            |count: usize| text.line_to_byte(count.saturating_sub(1).min(text.len_lines() - 1));
        if after_g && stroke != g && !rows {
            return Keyed::Text(before);
        } else if (after_g && !rows) || stroke == Stroke::char('G') {
            // To the block holding line `count`, or to the top or the end.
            match (count, after_g) {
                (Some(count), _) => self.go_to = Some(line(count)),
                (None, true) => self.scroll_to_top(),
                (None, false) => {
                    self.go_to = None;
                    self.scroll = self.most;
                }
            }
        } else if let Some(lines) = self.lines_for(stroke, count) {
            self.scroll_by(lines * self.line_height);
        } else if stroke != Stroke::Escape {
            return Keyed::Text(before);
        }
        Keyed::Took
    }

    /// How many lines `stroke` scrolls the view down, or up if fewer than
    /// none, with `count` typed, if it's one of the keys that scroll: `j`
    /// and `k` a line a time, and the scrolling keys as far as they go in
    /// any pane (see [`vim::Paging`]).
    fn lines_for(&mut self, stroke: Stroke, count: Option<usize>) -> Option<f32> {
        let plain = |ch| Stroke::Char { ch, ctrl: false };
        let ctrl = |ch| Stroke::Char { ch, ctrl: true };
        let keys = [
            (Stroke::Down, vim::Scroll::Line { down: true }),
            (plain('j'), vim::Scroll::Line { down: true }),
            (ctrl('e'), vim::Scroll::Line { down: true }),
            (Stroke::Up, vim::Scroll::Line { down: false }),
            (plain('k'), vim::Scroll::Line { down: false }),
            (ctrl('y'), vim::Scroll::Line { down: false }),
            (ctrl('d'), vim::Scroll::HalfPage { down: true }),
            (ctrl('u'), vim::Scroll::HalfPage { down: false }),
            (ctrl('f'), vim::Scroll::Page { down: true }),
            (Stroke::PageDown, vim::Scroll::Page { down: true }),
            (Stroke::ShiftDown, vim::Scroll::Page { down: true }),
            (Stroke::PageUp, vim::Scroll::Page { down: false }),
            (Stroke::ShiftUp, vim::Scroll::Page { down: false }),
        ];
        let &(_, scroll) = keys.iter().find(|(key, _)| *key == stroke)?;
        let (vim::Scroll::Line { down }
        | vim::Scroll::HalfPage { down }
        | vim::Scroll::Page { down }) = scroll;
        let view = self.view_rows();
        #[allow(clippy::cast_precision_loss, reason = "rows in view")]
        let rows = self.paging.rows(scroll, count, view) as f32;
        Some(if down { rows } else { -rows })
    }

    /// How many lines of its text the view shows, as drawn last.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rows in view"
    )]
    fn view_rows(&self) -> u64 {
        (self.view_height / self.line_height).floor().max(1.0) as u64
    }

    /// How far down the page the view is, in pixels.
    #[cfg(test)]
    pub(super) fn scrolled(&self) -> f32 {
        self.scroll
    }

    /// Scrolls the view by `rows` lines of text, up if more than none, as
    /// the mouse's wheel does.
    #[allow(clippy::cast_precision_loss, reason = "a few rows")]
    pub(super) fn wheel(&mut self, rows: i64) {
        self.scroll_by(-(rows as f32) * self.line_height);
    }

    /// Scrolls the view down `pixels`, or up if fewer than none, as far as
    /// the page goes.
    fn scroll_by(&mut self, pixels: f32) {
        self.go_to = None;
        self.scroll = (self.scroll + pixels).clamp(0.0, self.most.max(0.0));
    }

    fn scroll_to_top(&mut self) {
        self.go_to = None;
        self.scroll = 0.0;
    }

    /// Where the view is in the page, as Vim's ruler says it: `Top`, `Bot`,
    /// `All` or how far down, as `42%`.
    pub(super) fn position(&self) -> String {
        if self.most <= 0.0 {
            "All".to_owned()
        } else if self.scroll <= 0.0 {
            "Top".to_owned()
        } else if self.scroll >= self.most {
            "Bot".to_owned()
        } else {
            format!("{:.0}%", self.scroll / self.most * 100.0)
        }
    }

    /// The block at the top of the view, as laid out in `page`, by its
    /// index: the first with some of it below where the first line is with
    /// the view at the page's top. What's above that, in the margin over
    /// the page, only peeks in.
    fn top_block(&self, page: &Page) -> usize {
        let blocks = &page.laid.blocks;
        (blocks.iter())
            .position(|block| block.top + block.layout.height() > self.scroll)
            .unwrap_or(blocks.len().saturating_sub(1))
    }

    /// Whether byte `at` of the text is in the block at the top of the
    /// view, as drawn last.
    fn top_holds(&self, at: usize) -> bool {
        let Some(page) = &self.page else {
            return false;
        };
        let blocks = &page.laid.blocks;
        let top = self.top_block(page);
        let next = blocks.get(top + 1).map_or(usize::MAX, |block| block.at);
        blocks
            .get(top)
            .is_some_and(|block| block.at <= at && at < next)
    }

    /// Where in the text the blocks in view start, as drawn last: the one
    /// at the top, and the last; or nothing, if it hasn't been drawn.
    pub(super) fn shown_bytes(&self) -> Option<(usize, usize)> {
        let page = self.page.as_ref()?;
        let blocks = &page.laid.blocks;
        let first = blocks.get(self.top_block(page))?;
        let bottom = self.scroll - self.margin + self.view_height;
        let last = (blocks.iter())
            .rev()
            .find(|block| block.top < bottom)
            .unwrap_or(first);
        Some((first.at, last.at.max(first.at)))
    }

    /// Scrolls to the heading `fragment` names, as GitHub names headings:
    /// its words in lowercase, without punctuation, a `-` for each space,
    /// with `-1`, `-2` and so on after the same name again. Returns whether
    /// there's one.
    pub(super) fn go_to_heading(&mut self, fragment: &str) -> bool {
        let Some(page) = &self.page else {
            return false;
        };
        let mut named: Vec<String> = Vec::new();
        for block in &page.laid.blocks {
            if !matches!(block.kind, BlockKind::Heading(_)) {
                continue;
            }
            let slug = slug(&block.text);
            let again = named.iter().filter(|name| **name == slug).count();
            let name = match again {
                0 => slug.clone(),
                again => format!("{slug}-{again}"),
            };
            if name == fragment.to_lowercase() {
                let top = block.top;
                self.go_to = None;
                self.scroll = top.clamp(0.0, self.most.max(0.0));
                return true;
            }
            named.push(slug);
        }
        false
    }

    /// Where the link at `at`, from the pane's top left, goes, as drawn
    /// last, if there's one there: the outermost, as the link round an
    /// image.
    pub(super) fn link_at(&self, at: [f32; 2]) -> Option<&str> {
        let page = self.page.as_ref()?;
        self.shown
            .iter()
            .find_map(|&(index, origin, [left, top, width, height])| {
                let drawn =
                    (left..left + width).contains(&at[0]) && (top..top + height).contains(&at[1]);
                let block = page.laid.blocks.get(index).filter(|_| drawn)?;
                let (x, y) = (at[0] - origin[0], at[1] - origin[1]);
                let layout = &block.layout;
                if x < 0.0 || y < 0.0 || x > layout.width() || y > layout.height() {
                    return None;
                }
                let byte = parley::Cursor::from_point(layout, x, y).index();
                (block.links.iter())
                    .rev()
                    .find(|link| link.range.contains(&byte))
                    .map(|link| link.to.as_str())
            })
    }

    /// Lays the text out, if it's changed, or how it's drawn has, and draws
    /// it in a pane `size` pixels whose top left is at `origin`.
    pub(super) fn paint(
        &mut self,
        fonts: &mut Fonts,
        origin: [f32; 2],
        size: [f32; 2],
        copy: &BufferCopy,
    ) -> TextPaint {
        let em = text_size(fonts);
        let m = Measures::of(em);
        let rows = self.view_rows();
        self.line_height = (em * LINE).round();
        self.view_height = size[1];
        // A new size, or text size, forgets the half page a count set, as
        // Vim's `scroll` is set afresh when the window's height changes.
        if self.view_rows() != rows {
            self.paging.resized();
        }
        self.margin = m.em(1.5);
        let column = (size[0] - 2.0 * m.em(2.0)).clamp(m.em(8.0), m.em(COLUMN));
        let left = ((size[0] - column) / 2.0).max(0.0).round();
        self.lay_out(fonts, m, column, copy);
        let mut paint = TextPaint::default();
        let Some(page) = &self.page else {
            return paint;
        };
        if let Some(at) = self.go_to.take() {
            // The block holding it: the last that starts before it.
            let blocks = &page.laid.blocks;
            let holding = blocks.partition_point(|block| block.at <= at);
            self.scroll = blocks
                .get(holding.saturating_sub(1))
                .map_or(0.0, |block| block.top);
        }
        // The page ends as far below its last line as it starts above its
        // first.
        self.most = (page.laid.height + 2.0 * self.margin - size[1]).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.most);
        let top = [origin[0] + left, origin[1] + self.margin - self.scroll];
        let mut canvas = Canvas {
            paint: &mut paint,
            fonts,
            clip: [origin[0], origin[1], size[0], size[1]],
            em,
            over: false,
        };
        draw_laid(&mut canvas, m, &page.laid, top, column);
        // What has links, and is in view, for clicks.
        let (from, to) = (
            self.scroll - self.margin,
            self.scroll - self.margin + size[1],
        );
        self.shown = (page.laid.blocks.iter().enumerate())
            .filter(|(_, block)| !block.links.is_empty())
            .filter(|(_, block)| block.top < to && block.top + block.layout.height() > from)
            .map(|(index, block)| {
                let at = [left + block.left, self.margin - self.scroll + block.top];
                // A table's cells, as far as the column goes.
                let clip = match page.laid.table_of(index) {
                    Some(table) => {
                        let table = &page.laid.tables[table];
                        [left + table.left, 0.0, column - table.left, size[1]]
                    }
                    None => [0.0, 0.0, size[0], size[1]],
                };
                (index, at, clip)
            })
            .collect();
        paint
    }

    /// Lays the text out for a column `column` wide, if it's changed since
    /// it was, or how it's drawn has, keeping each block laid out before
    /// just so, and the block at the top of the view there.
    fn lay_out(&mut self, fonts: &mut Fonts, m: Measures, column: f32, copy: &BufferCopy) {
        let revision = copy.revision();
        let laid_for = |page: &Page| {
            page.revision == revision
                && page.width.to_bits() == column.to_bits()
                && page.size.to_bits() == m.em.to_bits()
                && page.fonts == fonts.generation
        };
        if !std::mem::take(&mut self.recoloured) && self.page.as_ref().is_some_and(laid_for) {
            return;
        }
        // The block at the top of the view, and how far into it the view
        // is, to find again.
        let anchor = self.page.as_ref().and_then(|page| {
            let index = self.top_block(page);
            let block = page.laid.blocks.get(index)?;
            Some((index, block.key, self.scroll - block.top))
        });
        let mut cells = match self.page.take() {
            Some(page) if page.fonts == fonts.generation => page.laid.into_cells(),
            _ => Cells::new(),
        };
        let text = copy.text().to_string();
        let blocks = markdown::blocks(&text);
        let laid = lay_out_blocks(
            &mut self.layout_cx,
            fonts,
            m,
            (blocks, String::new(), Look::Page),
            (column, &mut cells),
            Some(&mut self.colours),
        );
        #[cfg(test)]
        {
            self.fresh = laid.fresh;
        }
        if let Some((index, key, offset)) = anchor {
            self.scroll = found_again(&laid, index, key).map_or(self.scroll, |top| top + offset);
        }
        self.page = Some(Page {
            revision,
            width: column,
            size: m.em,
            fonts: fonts.generation,
            laid,
        });
    }

    /// The code laid out whose colours are wanted from the daemon, in asks
    /// no bigger than it colours.
    pub(super) fn take_colour_asks(&mut self) -> Vec<Vec<Code>> {
        self.colours.take_wanted()
    }

    /// `code` was asked for, numbered `id`; or with none, can't be, as the
    /// daemon doesn't colour code, and is plain.
    pub(super) fn colours_asked(&mut self, id: Option<u64>, code: Vec<Code>) {
        match id {
            Some(id) => self.colours.asked(id, code),
            None => self.colours.plain(code),
        }
        self.recoloured |= id.is_none();
    }

    /// The daemon's colours for the ask numbered `id`. Returns whether they
    /// were the view's.
    pub(super) fn coloured(&mut self, id: u64, spans: Vec<Vec<Span>>) -> bool {
        let ours = self.colours.answered(id, spans);
        self.recoloured |= ours;
        ours
    }

    /// The asks for colours not answered never will be, as the connection
    /// was lost: they're asked again.
    pub(super) fn forget_colour_asks(&mut self) {
        self.colours.forget_asked();
        self.recoloured = true;
    }
}

/// The top of the block laid out in `laid` with `key`, the one nearest
/// `index` if several have it, or with none, of the block at `index`, or
/// the last: where a block at the top of a view before is now.
fn found_again(laid: &Laid, index: usize, key: u64) -> Option<f32> {
    let blocks = &laid.blocks;
    let near = (0..blocks.len()).flat_map(|distance| {
        let below = index.checked_add(distance);
        let above = index.checked_sub(distance).filter(|_| distance > 0);
        below.into_iter().chain(above)
    });
    near.filter_map(|at| blocks.get(at))
        .find(|block| block.key == key)
        .or_else(|| blocks.get(index).or(blocks.last()))
        .map(|block| block.top)
}

/// The digit `stroke` types, if it types one.
fn digit(stroke: Stroke) -> Option<char> {
    match stroke {
        Stroke::Char { ch, ctrl: false } if ch.is_ascii_digit() => Some(ch),
        _ => None,
    }
}

/// What GitHub names a heading of `text` for links to it: its words in
/// lowercase, without punctuation but `-` and `_`, a `-` for each space.
pub(super) fn slug(text: &str) -> String {
    (text.chars())
        .filter_map(|ch| match ch {
            ' ' => Some('-'),
            '-' | '_' => Some(ch),
            _ if ch.is_alphanumeric() => Some(ch),
            _ => None,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether the file at `path` is Markdown, by its name's ending, as the
/// editor knows languages by: `.md`, `.markdown` and their kin.
pub(crate) fn is_markdown(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["md", "markdown", "mdown", "mkd", "mkdn"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

impl Editor {
    /// Whether it shows its text rendered, as Markdown.
    pub(crate) fn rendered(&self) -> bool {
        self.rendered.is_some()
    }

    /// Whether its file is one it can render: Markdown, and not a lesson's
    /// or a listing's, which are the window's own.
    pub(super) fn renders(&self, copy: &BufferCopy) -> bool {
        !self.lesson && !copy.is_scratch() && is_markdown(copy.path())
    }

    /// Shows the text rendered, or the text itself if it's rendered, as
    /// `:markdown` does, keeping the place, and says which in the bar; or
    /// says why it can't: the file isn't Markdown.
    pub(crate) fn toggle_rendered(&mut self, copy: &mut BufferCopy) -> Result<Done, String> {
        if self.rendered() {
            self.show_text(copy);
            self.status = Some("raw Markdown".into());
            return Ok(Done::REDRAW);
        }
        if !self.renders(copy) {
            return Err(format!("‘{}’ isn't Markdown", copy.name()));
        }
        self.enter_normal_mode(copy);
        self.show_rendered(copy);
        self.status = Some("rendered Markdown".into());
        Ok(Done::REDRAW)
    }

    /// Shows the text rendered, with the block holding the line at the top
    /// of the view at the top. It's in normal mode. The view put away last,
    /// if there is one, is shown again, laying out only what's changed.
    fn show_rendered(&mut self, copy: &BufferCopy) {
        self.leave();
        self.panel = None;
        let rope = copy.text();
        let top = usize::try_from(self.top.line).unwrap_or(usize::MAX);
        let at = rope.line_to_byte(top.min(rope.len_lines().saturating_sub(1)));
        let mut rendered = (self.put_away.take()).unwrap_or_else(|| Box::new(Rendered::new(at)));
        rendered.open_at(at);
        self.rendered = Some(rendered);
    }

    /// Shows the text itself again, if it's rendered, as `:markdown` does:
    /// with the line the top block starts on at the top, or as it was, if
    /// the top block holds the line that was at the top. The cursor stays
    /// where it was if that's in view, and else goes to that line.
    pub(crate) fn show_text(&mut self, copy: &BufferCopy) {
        self.leave_rendered(copy, false);
    }

    /// Shows the text itself again, if it's rendered, to edit it where the
    /// view was, as a key that isn't the view's does, or a paste: with the
    /// cursor at the start of the line the top block starts on, at the top.
    pub(crate) fn show_text_to_edit(&mut self, copy: &BufferCopy) {
        self.leave_rendered(copy, true);
    }

    /// Shows the text itself again, if it's rendered, as
    /// [`show_text`](Self::show_text) does, or with `to_edit`, as
    /// [`show_text_to_edit`](Self::show_text_to_edit) does.
    fn leave_rendered(&mut self, copy: &BufferCopy, to_edit: bool) {
        let Some(rendered) = self.rendered.take() else {
            return;
        };
        let rendered = &**self.put_away.insert(rendered);
        let rope = copy.text();
        let was = usize::try_from(self.top.line).unwrap_or(usize::MAX);
        let was = rope.line_to_byte(was.min(rope.len_lines().saturating_sub(1)));
        if !to_edit && rendered.top_holds(was) {
            return;
        }
        let Some((first, last)) = rendered.shown_bytes() else {
            return;
        };
        let line = |byte: usize| rope.byte_to_line(byte.min(rope.len_bytes())) as u64;
        let (top, bottom) = (line(first), line(last));
        if !to_edit && (top..=bottom).contains(&self.cursor.at.line) {
            self.top = super::wrap::Top { line: top, cell: 0 };
            let text = self.text(copy);
            self.cursor_into_view(&text);
        } else {
            self.go_to_top(top, copy);
        }
    }

    /// Lays out and draws the text rendered, if it's shown so, in a pane
    /// `size` pixels whose top left is at `origin`.
    pub(crate) fn paint_rendered(
        &mut self,
        fonts: &mut Fonts,
        origin: [f32; 2],
        size: [f32; 2],
        copy: &BufferCopy,
    ) -> Option<TextPaint> {
        let rendered = self.rendered.as_mut()?;
        Some(rendered.paint(fonts, origin, size, copy))
    }

    /// Has a key typed while the text is rendered do what it does: scroll
    /// the view, or, if it's not one of the view's, have the text shown,
    /// and the keys typed before it typed there. Returns what it did, if
    /// the view took it.
    pub(super) fn rendered_key(
        &mut self,
        stroke: Stroke,
        copy: &mut BufferCopy,
        shared: &mut Shared,
    ) -> Option<Done> {
        let rendered = self.rendered.as_mut()?;
        // What the bar said goes, as with any key.
        self.status = None;
        match rendered.key(stroke, &mut self.keys, copy.text()) {
            Keyed::Took => Some(Done::REDRAW),
            Keyed::Text(before) => {
                self.show_text_to_edit(copy);
                for stroke in before {
                    self.stroke(stroke, copy, shared);
                }
                None
            }
        }
    }

    /// Scrolls the view of the text rendered by `rows` lines, up if more
    /// than none, as the wheel does. Returns whether it's rendered.
    pub(crate) fn rendered_wheel_metrics(&self) -> Option<(f32, f32, f32)> {
        self.rendered
            .as_ref()
            .map(|r| (r.scroll, r.most, r.line_height))
    }

    pub(crate) fn scroll_rendered_pixels(&mut self, pixels: f32) -> f32 {
        let Some(rendered) = &mut self.rendered else {
            return 0.0;
        };
        let old = rendered.scroll;
        rendered.scroll_by(pixels);
        rendered.scroll - old
    }

    pub(crate) fn wheel_rendered(&mut self, rows: i64) -> bool {
        let Some(rendered) = &mut self.rendered else {
            return false;
        };
        rendered.wheel(rows);
        true
    }

    /// Where the link at `at`, from the pane's top left, of the text
    /// rendered goes, if there's one there.
    pub(crate) fn rendered_link_at(&self, at: [f32; 2]) -> Option<&str> {
        self.rendered.as_ref()?.link_at(at)
    }

    /// Scrolls the text rendered to the heading `fragment` names. Returns
    /// whether there's one.
    pub(crate) fn rendered_heading(&mut self, fragment: &str) -> bool {
        (self.rendered.as_mut()).is_some_and(|rendered| rendered.go_to_heading(fragment))
    }

    /// Where the text rendered is in view, for the bar, if it's rendered.
    pub(crate) fn rendered_position(&self) -> Option<String> {
        self.rendered.as_ref().map(|rendered| rendered.position())
    }

    /// Turns the rendered view on if files open so (`markdownview`), once
    /// the file's text has come and it turns out to be Markdown: once, and
    /// only in normal mode, as a pane the focus arrives at in insert mode
    /// is to be typed in.
    pub(super) fn open_as_wanted(&mut self, copy: &BufferCopy) {
        if !self.open_rendered || !copy.loaded() {
            return;
        }
        self.open_rendered = false;
        // Nothing half typed, which showing it rendered would throw away,
        // leaving the rest of its keys to edit the text: else it stays as
        // its text.
        let idle = self.mode == Mode::Normal && !self.keys.pending() && self.prompt.is_none();
        if self.renders(copy) && idle {
            self.show_rendered(copy);
        }
    }

    /// The code the text rendered laid out whose colours are wanted, in
    /// asks no bigger than the daemon colours.
    pub(crate) fn take_colour_asks(&mut self) -> Vec<Vec<Code>> {
        (self.rendered.as_mut()).map_or_else(Vec::new, |rendered| rendered.take_colour_asks())
    }

    /// `code` was asked for, numbered `id`, or can't be, with none.
    pub(crate) fn colours_asked(&mut self, id: Option<u64>, code: Vec<Code>) {
        if let Some(rendered) = &mut self.rendered {
            rendered.colours_asked(id, code);
        }
    }

    /// The daemon's colours for the ask numbered `id`. Returns whether they
    /// were for the text rendered, if it's shown.
    pub(crate) fn coloured(&mut self, id: u64, spans: Vec<Vec<Span>>) -> bool {
        let asked = (self.rendered.iter_mut().chain(&mut self.put_away))
            .find(|rendered| rendered.colours.waits_for(id));
        asked.is_some_and(|rendered| rendered.coloured(id, spans)) && self.rendered()
    }

    /// Forgets the asks for colours not answered, as the connection's gone.
    pub(crate) fn forget_colour_asks(&mut self) {
        for rendered in self.rendered.iter_mut().chain(&mut self.put_away) {
            rendered.forget_colour_asks();
        }
    }
}
