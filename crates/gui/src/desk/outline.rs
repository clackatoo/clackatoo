//! A per-editor outline beside the text. All source reads stay on the daemon.

use super::Desk;
use crate::layout::Tile;
use crate::vim::Stroke;
use clackatoo_protocol::{BufferId, Done, Found, FoundSymbol, PaneId, Search, Size, wire};
use clackatoo_term::{Attributes, Color, Row, RowBuilder, Style};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const PAUSE: Duration = Duration::from_millis(180);

#[derive(Debug)]
pub(super) struct Outline {
    buffer: BufferId,
    revision: u64,
    path: String,
    pending: Option<u64>,
    due: Option<Instant>,
    refresh: bool,
    symbols: Vec<FoundSymbol>,
    ends: Vec<u64>,
    selected: usize,
    top: usize,
    wheel: f64,
    focused: bool,
    message: String,
    width: u16,
    height: u16,
    rows: Vec<Row>,
}

impl Outline {
    fn new(buffer: BufferId, revision: u64, path: String) -> Self {
        Self {
            buffer,
            revision,
            path,
            pending: None,
            due: Some(Instant::now()),
            refresh: false,
            symbols: Vec::new(),
            ends: Vec::new(),
            selected: 0,
            top: 0,
            wheel: 0.0,
            focused: true,
            message: "Reading source…".into(),
            width: 0,
            height: 0,
            rows: Vec::new(),
        }
    }

    fn draw(&mut self) {
        self.rows.clear();
        if self.width == 0 || self.height < 2 {
            return;
        }
        let heading = Style {
            fg: Color::Indexed(4),
            attributes: Attributes::BOLD,
            ..Style::DEFAULT
        };
        let mut header = RowBuilder::new(usize::from(self.width));
        let label = if self.focused {
            "│ Outline •"
        } else {
            "│ Outline"
        };
        let padding = " ".repeat(usize::from(self.width).saturating_sub(label.chars().count() + 1));
        header.push(&format!("{label}{padding}×"), &heading, true);
        self.rows.push(header.finish(false));
        let height = usize::from(self.height.saturating_sub(2));
        if self.selected < self.top {
            self.top = self.selected;
        }
        if self.selected >= self.top + height {
            self.top = self.selected.saturating_add(1).saturating_sub(height);
        }
        for at in self.top..(self.top + height).min(self.symbols.len()) {
            let symbol = &self.symbols[at].symbol;
            let indent = "  ".repeat(usize::from(symbol.depth.min(4)));
            let kind = match symbol.kind {
                12 => "ƒ",
                5 => "C",
                2 => "M",
                11 => "I",
                14 => "K",
                _ => "T",
            };
            let style = if at == self.selected {
                Style {
                    fg: Color::Indexed(6),
                    attributes: Attributes::BOLD,
                    ..Style::DEFAULT
                }
            } else {
                Style::DEFAULT
            };
            let mut row = RowBuilder::new(usize::from(self.width));
            row.push(&format!("│ {indent}{kind} {}", symbol.name), &style, true);
            self.rows.push(row.finish(false));
        }
        while self.rows.len() + 1 < usize::from(self.height) {
            let mut row = RowBuilder::new(usize::from(self.width));
            row.push("│", &Style::DEFAULT, true);
            self.rows.push(row.finish(false));
        }
        let mut footer = RowBuilder::new(usize::from(self.width));
        footer.push(
            &format!("│ {}", self.message),
            &Style {
                fg: Color::Indexed(8),
                ..Style::DEFAULT
            },
            true,
        );
        self.rows.push(footer.finish(false));
    }
}

pub(super) fn width(cols: u16) -> u16 {
    if cols < 64 {
        0
    } else {
        (cols / 3).clamp(24, 40)
    }
}

impl Desk {
    pub(super) fn toggle_outline(&mut self, focus: bool) {
        if !self.needs_daemon("code outlines", wire::CODE_OUTLINES) {
            return;
        }
        let Some(pane) = self.shown.map(|shown| shown.pane) else {
            return;
        };
        let Some(editor) = self.editors.panes.get(&pane) else {
            return self.refuse("Open a source file to show its outline.".into());
        };
        let buffer = editor.buffer();
        let Some(copy) = self
            .editors
            .buffers
            .get(&buffer)
            .filter(|copy| !copy.is_blank() && !copy.is_scratch())
        else {
            return self.refuse("Save this buffer before opening its outline.".into());
        };
        let revision = copy.revision();
        let path = copy.path().to_owned();
        if focus && let Some(outline) = self.outlines.get_mut(&pane) {
            outline.focused = true;
            outline.draw();
            return;
        }
        if let Some(outline) = self.outlines.remove(&pane) {
            if let Some(id) = outline.pending {
                self.stop_search(id);
            }
        } else {
            self.outlines
                .insert(pane, Outline::new(buffer, revision, path));
        }
        self.fit_panes();
        if self
            .outlines
            .get(&pane)
            .is_some_and(|outline| outline.width == 0)
        {
            self.say("Outline is hidden until this pane is wider.".into());
        }
        self.tick_outlines(Instant::now());
    }

    pub(super) fn fit_outline(&mut self, pane: PaneId, size: Size) -> Size {
        let Some(outline) = self.outlines.get_mut(&pane) else {
            return size;
        };
        outline.width = width(size.cols());
        if outline.width == 0 {
            outline.focused = false;
        }
        outline.height = size.rows();
        outline.draw();
        Size::new(size.cols() - outline.width, size.rows()).unwrap_or(size)
    }

    pub(crate) fn outline_tile(&self, pane: PaneId, mut tile: Tile) -> Option<Tile> {
        let outline = self
            .outlines
            .get(&pane)
            .filter(|outline| outline.width > 0)?;
        let cell = self.area.as_ref()?.cell[0];
        tile.origin[0] += f32::from(tile.cols - outline.width) * cell;
        tile.rect.x = tile.origin[0];
        tile.rect.width = f32::from(outline.width) * cell;
        tile.cols = outline.width;
        Some(tile)
    }

    pub(crate) fn editor_tile(&self, pane: PaneId, mut tile: Tile) -> Tile {
        if let Some(side) = self.outline_tile(pane, tile) {
            tile.cols -= side.cols;
            tile.rect.width = side.rect.x - tile.rect.x;
        }
        tile
    }

    pub(crate) fn outline_focused(&self, pane: PaneId) -> bool {
        self.outlines
            .get(&pane)
            .is_some_and(|outline| outline.focused && outline.width > 0)
    }

    pub(crate) fn outline_rows(&self, pane: PaneId) -> Vec<&Row> {
        self.outlines
            .get(&pane)
            .map(|outline| outline.rows.iter().collect())
            .unwrap_or_default()
    }

    pub(super) fn tick_outlines(&mut self, now: Instant) -> bool {
        if !self.connected {
            return false;
        }
        self.outlines
            .retain(|pane, _| self.editors.panes.contains_key(pane));
        let visible: Vec<_> = self
            .shown_tab()
            .map(|tab| tab.layout.panes().iter().map(|pane| pane.id).collect())
            .unwrap_or_default();
        let mut requests = Vec::new();
        let mut stopped = Vec::new();
        let mut changed = false;
        for (&pane, outline) in &mut self.outlines {
            if !visible.contains(&pane) {
                continue;
            }
            let Some(editor) = self.editors.panes.get(&pane) else {
                continue;
            };
            let Some(copy) = self.editors.buffers.get(&editor.buffer()) else {
                continue;
            };
            if outline.buffer != editor.buffer()
                || outline.revision != copy.revision()
                || outline.path != copy.path()
            {
                if let Some(id) = outline.pending.take() {
                    stopped.push(id);
                }
                outline.buffer = editor.buffer();
                outline.revision = copy.revision();
                outline.path = copy.path().into();
                outline.symbols.clear();
                outline.ends.clear();
                outline.message = "Source changed…".into();
                outline.due = Some(now + PAUSE);
                outline.refresh = true;
                outline.draw();
                changed = true;
            }
            if !copy.loaded() {
                continue;
            }
            if !outline.focused {
                let line = editor.spot().0.line;
                let at = outline
                    .symbols
                    .iter()
                    .zip(&outline.ends)
                    .rposition(|(symbol, end)| symbol.symbol.line <= line && line <= *end)
                    .or_else(|| {
                        outline
                            .symbols
                            .iter()
                            .rposition(|symbol| symbol.symbol.line <= line)
                    })
                    .unwrap_or(0);
                if at != outline.selected {
                    outline.selected = at;
                    outline.draw();
                    changed = true;
                }
            }
            if outline.pending.is_none() && outline.due.is_some_and(|due| now >= due) {
                outline.due = None;
                requests.push((pane, outline.refresh));
            }
        }
        for id in stopped {
            self.stop_search(id);
        }
        for (pane, refresh) in requests {
            let id = self.send_search_from(
                Some(pane),
                Search::CodeSymbols {
                    query: String::new(),
                    file: true,
                    refresh,
                },
            );
            self.outlines.get_mut(&pane).expect("listed").pending = Some(id);
            changed = true;
        }
        changed
    }

    pub(super) fn reconnect_outlines(&mut self) {
        for outline in self.outlines.values_mut() {
            outline.pending = None;
            outline.refresh = true;
            outline.due = Some(Instant::now());
        }
    }

    pub(super) fn outline_due(&self) -> Option<Instant> {
        if !self.connected {
            return None;
        }
        let shown = self.shown_tab()?;
        self.outlines
            .iter()
            .filter(|(pane, _)| shown.layout.panes().iter().any(|shown| shown.id == **pane))
            .filter(|(pane, _)| {
                self.editors
                    .panes
                    .get(pane)
                    .and_then(|editor| self.editors.buffers.get(&editor.buffer()))
                    .is_some_and(crate::editor::BufferCopy::loaded)
            })
            .filter_map(|(_, outline)| outline.due)
            .min()
    }

    pub(super) fn outline_found(&mut self, id: u64, found: &Found) -> bool {
        // A response can arrive before the debounce tick sees a local edit.
        self.tick_outlines(Instant::now());
        let Some(outline) = self
            .outlines
            .values_mut()
            .find(|outline| outline.pending == Some(id))
        else {
            return false;
        };
        match found {
            Found::Symbols(symbols) => {
                outline.symbols.clone_from(symbols);
                outline.selected = outline.selected.min(symbols.len().saturating_sub(1));
            }
            Found::IndexStatus { message, ends, .. } => {
                outline.ends.clone_from(ends);
                if !message.is_empty() {
                    outline.message.clone_from(message);
                }
            }
            Found::Done(done) => {
                outline.pending = None;
                outline.message = match done {
                    Done::Failed(why) => why.clone(),
                    Done::CutShort => "More symbols than shown".into(),
                    Done::All => format!("{} symbols · r:refresh", outline.symbols.len()),
                };
                if matches!(done, Done::Failed(why) if why.starts_with("Source changed")) {
                    outline.refresh = true;
                    outline.due = Some(Instant::now() + PAUSE);
                }
            }
            _ => {}
        }
        outline.draw();
        true
    }

    pub(super) fn outline_press(&mut self, pane: PaneId, [row, col]: [i64; 2]) -> bool {
        let Some(tile) = self.placement().and_then(|placement| {
            placement
                .panes
                .into_iter()
                .find(|(id, _)| *id == pane)
                .map(|(_, tile)| tile)
        }) else {
            return false;
        };
        let Some(outline) = self
            .outlines
            .get_mut(&pane)
            .filter(|outline| outline.width > 0)
        else {
            return false;
        };
        if col < i64::from(tile.cols - outline.width) {
            return false;
        }
        if row == 0 {
            // The heading's rightmost cross closes it; elsewhere focuses it.
            if col >= i64::from(tile.cols.saturating_sub(2)) {
                self.toggle_outline(false);
            } else {
                outline.focused = true;
                outline.draw();
            }
        } else if let Ok(row) = usize::try_from(row - 1) {
            let at = outline.top + row;
            if at < outline.symbols.len() && row < usize::from(outline.height.saturating_sub(2)) {
                outline.selected = at;
                self.jump_outline(pane);
            }
        }
        true
    }

    pub(super) fn outline_wheel(&mut self, pane: PaneId, point: [f32; 2], lines: i64) -> bool {
        let Some((_, tile)) = self
            .placement()
            .and_then(|placement| placement.panes.into_iter().find(|(id, _)| *id == pane))
        else {
            return false;
        };
        let Some(side) = self.outline_tile(pane, tile) else {
            return false;
        };
        if point[0] < side.rect.x {
            return false;
        }
        let outline = self.outlines.get_mut(&pane).expect("shown");
        let by = isize::try_from(lines.saturating_neg()).unwrap_or(0);
        outline.selected = outline
            .selected
            .saturating_add_signed(by)
            .min(outline.symbols.len().saturating_sub(1));
        outline.focused = true;
        outline.draw();
        true
    }

    pub(super) fn outline_smooth_wheel(
        &mut self,
        pane: PaneId,
        point: [f32; 2],
        lines: f64,
    ) -> Option<bool> {
        let (_, tile) = self
            .placement()?
            .panes
            .into_iter()
            .find(|(id, _)| *id == pane)?;
        if point[0] < self.outline_tile(pane, tile)?.rect.x {
            return None;
        }
        let outline = self.outlines.get_mut(&pane)?;
        outline.wheel = (outline.wheel + lines).clamp(-1000.0, 1000.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "A finite wheel remainder clamped to a thousand rows."
        )]
        let whole = outline.wheel.trunc() as i32;
        outline.wheel -= f64::from(whole);
        Some(whole != 0 && self.outline_wheel(pane, point, i64::from(whole)))
    }

    pub(super) fn outline_context(
        &self,
        pane: PaneId,
    ) -> Option<(clackatoo_protocol::ContextRef, String)> {
        let outline = self.outlines.get(&pane).filter(|o| o.focused)?;
        let symbol = outline.symbols.get(outline.selected)?;
        let first = u32::try_from(symbol.symbol.line).ok()?.checked_add(1)?;
        let last = u32::try_from(*outline.ends.get(outline.selected)?)
            .ok()?
            .checked_add(1)?;
        Some((
            clackatoo_protocol::ContextRef::Symbol {
                path: outline.path.clone(),
                name: symbol.symbol.name.clone(),
                lines: (first, last),
                signature: false,
            },
            symbol.symbol.name.clone(),
        ))
    }
    pub(super) fn outline_stroke(&mut self, pane: PaneId, stroke: Stroke) -> bool {
        let Some(outline) = self
            .outlines
            .get_mut(&pane)
            .filter(|outline| outline.focused && outline.width > 0)
        else {
            return false;
        };
        match stroke {
            Stroke::Escape
            | Stroke::Char {
                ch: 'h',
                ctrl: false,
            } => outline.focused = false,
            Stroke::Char {
                ch: 'r',
                ctrl: false,
            } => {
                outline.refresh = false;
                outline.due = Some(Instant::now());
            }
            Stroke::Enter => {
                self.jump_outline(pane);
                return true;
            }
            Stroke::Char {
                ch: 'q',
                ctrl: false,
            } => {
                self.toggle_outline(false);
                return true;
            }
            Stroke::Down
            | Stroke::Char {
                ch: 'j',
                ctrl: false,
            } => {
                outline.selected =
                    (outline.selected + 1).min(outline.symbols.len().saturating_sub(1));
            }
            Stroke::Up
            | Stroke::Char {
                ch: 'k',
                ctrl: false,
            } => outline.selected = outline.selected.saturating_sub(1),
            Stroke::Home => outline.selected = 0,
            Stroke::End => outline.selected = outline.symbols.len().saturating_sub(1),
            Stroke::PageDown => {
                outline.selected = (outline.selected
                    + usize::from(outline.height.saturating_sub(2)))
                .min(outline.symbols.len().saturating_sub(1));
            }
            Stroke::PageUp => {
                outline.selected = outline
                    .selected
                    .saturating_sub(usize::from(outline.height.saturating_sub(2)));
            }
            Stroke::Char { ch: ':', .. } => {
                outline.focused = false;
                outline.draw();
                return false;
            }
            _ => return true,
        }
        outline.draw();
        true
    }

    fn jump_outline(&mut self, pane: PaneId) {
        let Some(outline) = self.outlines.get_mut(&pane) else {
            return;
        };
        let Some(symbol) = outline.symbols.get(outline.selected) else {
            return;
        };
        let (line, col) = (symbol.symbol.line, symbol.symbol.col);
        outline.focused = false;
        outline.draw();
        self.record_jump(pane, None);
        if let Some((editor, copy)) = self.editor_and_copy(pane) {
            editor.go_to(line, col, copy);
        }
    }
}

pub(super) type Outlines = HashMap<PaneId, Outline>;
