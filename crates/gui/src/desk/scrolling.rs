//! Wheel motion belongs to a pane, never to wherever the pointer happens to
//! be on the next frame. Grid panes draw one extra row while between rows.
use std::time::Instant;

use super::{Desk, Modifiers, PaneId, TerminalId};
use crate::render::RowScroll;
use crate::smooth::Motion;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Rows,
    Chat(TerminalId),
    Markdown,
}

#[derive(Debug)]
pub(super) struct Gesture {
    kind: Kind,
    modifiers: Modifiers,
    motion: Motion,
}

impl Desk {
    /// None means the original wheel event belongs to a program or popup.
    pub(crate) fn smooth_wheel(
        &mut self,
        point: [f32; 2],
        lines: f64,
        precise: bool,
        modifiers: Modifiers,
    ) -> Option<bool> {
        if !lines.is_finite() || lines == 0.0 {
            return Some(false);
        }
        let pane = self.placement()?.pane_at(point)?;
        let (kind, room, unit) = self.scroll_target(pane, lines > 0.0, modifiers)?;
        let mut disarmed = false;
        if let Kind::Chat(terminal) = kind
            && let Some(chat) = self.chats.get_mut(&terminal)
        {
            disarmed = chat.disarm();
        }
        if self.scrolling.get(&pane).is_some_and(|g| g.kind != kind) {
            self.scrolling.remove(&pane);
            self.scroll_extra(pane, false);
        }
        let reversing_fraction = lines > 0.0
            && self
                .scrolling
                .get(&pane)
                .is_some_and(|g| g.motion.position < 0.0);
        if !(room || reversing_fraction) {
            let stopped = self.scrolling.remove(&pane).is_some();
            self.scroll_extra(pane, false);
            return Some(stopped || disarmed);
        }
        let unit = if precise && !matches!(kind, Kind::Rows) {
            f64::from(self.area.as_ref()?.cell[1])
        } else {
            unit
        };
        let smooth = self.options.smoothscroll && !precise;
        let now = Instant::now();
        let gesture = self.scrolling.entry(pane).or_insert_with(|| Gesture {
            kind,
            modifiers,
            motion: Motion::default(),
        });
        gesture.modifiers = modifiers;
        gesture
            .motion
            .add(lines.clamp(-1000.0, 1000.0) * unit, smooth, now);
        self.tick_scrolling(now);
        Some(true)
    }

    fn scroll_target(
        &self,
        pane: PaneId,
        up: bool,
        modifiers: Modifiers,
    ) -> Option<(Kind, bool, f64)> {
        if let Some(editor) = self.editor(pane) {
            if let Some((at, end, line)) = editor.rendered_wheel_metrics() {
                return Some((
                    Kind::Markdown,
                    if up { at > 0.0 } else { at < end },
                    f64::from(line),
                ));
            }
            let copy = self.editors.buffers.get(&editor.buffer())?;
            return Some((Kind::Rows, editor.native_scroll_room(up, copy), 1.0));
        }
        if let Some(explorer) = self.explorer(pane) {
            return Some((Kind::Rows, explorer.native_scroll_room(up), 1.0));
        }
        if let Some(board) = self.board(pane) {
            return Some((Kind::Rows, board.native_scroll_room(up), 1.0));
        }
        if let Some(review) = self.review(pane) {
            return Some((Kind::Rows, review.native_scroll_room(up), 1.0));
        }
        let (_, _, layout) = self.workspaces.find_pane(pane)?;
        let terminal = layout.content.terminal()?;
        if let Some(chat) = self.chats.get(&terminal) {
            if modifiers.shift {
                return None;
            }
            let (at, end, line) = chat.view().wheel_metrics()?;
            return Some((
                Kind::Chat(terminal),
                if up { at > 0.0 } else { at < end },
                f64::from(line) * 3.0,
            ));
        }
        Some((
            Kind::Rows,
            self.panes
                .get(&terminal)?
                .native_scroll_room(up, modifiers.shift)?,
            1.0,
        ))
    }

    pub(crate) fn scroll_wakes(&self) -> Option<Instant> {
        self.scrolling
            .values()
            .filter_map(|gesture| gesture.motion.wakes())
            .min()
    }

    /// Animation frames do not create application input or network round trips.
    pub(crate) fn tick_scrolling(&mut self, now: Instant) -> bool {
        if self.scrolling.is_empty() {
            return false;
        }
        let visible = self.placement();
        let panes: Vec<_> = self.scrolling.keys().copied().collect();
        let mut changed = false;
        for pane in panes {
            let Some(mut gesture) = self.scrolling.remove(&pane) else {
                continue;
            };
            if !visible
                .as_ref()
                .is_some_and(|p| p.panes.iter().any(|(id, _)| *id == pane))
            {
                self.scroll_extra(pane, false);
                continue;
            }
            if self
                .scroll_target(pane, true, gesture.modifiers)
                .is_none_or(|(kind, _, _)| kind != gesture.kind)
            {
                self.scroll_extra(pane, false);
                changed = true;
                continue;
            }
            let before = gesture.motion.position;
            gesture.motion.advance(now);
            let position = gesture.motion.position;
            match gesture.kind {
                Kind::Rows => {
                    #[allow(clippy::cast_possible_truncation)]
                    let rows = position.ceil() as i64;
                    let moved = if rows == 0 {
                        0
                    } else {
                        self.scroll_rows(pane, rows)
                    };
                    if moved == rows {
                        #[allow(clippy::cast_precision_loss)]
                        gesture.motion.consume(rows as f64);
                        // At the bottom, a fractional trackpad step must not
                        // expose empty overscroll. Reversing still works at once.
                        if gesture.motion.position < 0.0
                            && self
                                .scroll_target(pane, false, gesture.modifiers)
                                .is_none_or(|(_, room, _)| !room)
                        {
                            gesture.motion = Motion::default();
                        }
                    } else {
                        gesture.motion = Motion::default();
                    }
                    self.scroll_extra(pane, gesture.motion.position < -0.000_001);
                    changed |= moved != 0 || (position - before).abs() > f64::EPSILON;
                }
                Kind::Chat(terminal) => {
                    let moved = self
                        .chats
                        .get_mut(&terminal)
                        .map(crate::chat::ChatPane::view_mut)
                        .map_or(0.0, |chat| {
                            let Some((before, _, _)) = chat.wheel_metrics() else {
                                return 0.0;
                            };
                            #[allow(clippy::cast_possible_truncation)]
                            chat.scroll_by(-position as f32);
                            before - chat.wheel_metrics().map_or(before, |m| m.0)
                        });
                    consume_pixels(&mut gesture.motion, position, moved);
                    changed |= moved.abs() > f32::EPSILON;
                }
                Kind::Markdown => {
                    let moved = self.editors.panes.get_mut(&pane).map_or(0.0, |editor| {
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            -editor.scroll_rendered_pixels(-position as f32)
                        }
                    });
                    consume_pixels(&mut gesture.motion, position, moved);
                    changed |= moved.abs() > f32::EPSILON;
                }
            }
            if !gesture.motion.idle() {
                self.scrolling.insert(pane, gesture);
            }
        }
        changed
    }

    fn scroll_rows(&mut self, pane: PaneId, rows: i64) -> i64 {
        if let Some(editor) = self.editors.panes.get_mut(&pane) {
            return self
                .editors
                .buffers
                .get(&editor.buffer())
                .map_or(0, |copy| editor.scroll_native(rows, copy));
        }
        if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
            return explorer.scroll_native(rows);
        }
        if let Some(board) = self.boards.panes.get_mut(&pane) {
            return board.scroll_native(rows);
        }
        if let Some(review) = self.reviews.panes.get_mut(&pane) {
            return review.scroll_native(rows);
        }
        self.workspaces
            .find_pane(pane)
            .and_then(|(_, _, p)| p.content.terminal())
            .and_then(|id| self.panes.get_mut(&id))
            .map_or(0, |p| p.scroll_native(rows))
    }

    fn scroll_extra(&mut self, pane: PaneId, extra: bool) {
        if let Some(editor) = self.editors.panes.get_mut(&pane) {
            editor.scroll_extra = usize::from(extra);
        }
        if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
            explorer.scroll_extra = usize::from(extra);
        }
        if let Some(board) = self.boards.panes.get_mut(&pane) {
            board.scroll_extra = usize::from(extra);
        }
        if let Some(review) = self.reviews.panes.get_mut(&pane) {
            review.scroll_extra = usize::from(extra);
        }
    }

    pub(crate) fn row_scroll(&self, pane: PaneId) -> Option<RowScroll> {
        let gesture = self.scrolling.get(&pane)?;
        if !matches!(gesture.kind, Kind::Rows) || gesture.motion.position >= -0.000_001 {
            return None;
        }
        let range =
            if let Some(explorer) = self.explorer(pane).filter(|_| self.editor(pane).is_none()) {
                explorer.scroll_region()
            } else if let Some(board) = self.board(pane) {
                board.scroll_region()
            } else if let Some(review) = self.review(pane) {
                review.scroll_region()
            } else {
                0..usize::from(
                    self.placement()?
                        .panes
                        .iter()
                        .find(|(id, _)| *id == pane)?
                        .1
                        .rows,
                )
            };
        #[allow(clippy::cast_possible_truncation)]
        Some(RowScroll {
            first: range.start,
            end: range.end,
            fraction: -gesture.motion.position as f32,
        })
    }

    /// Hit testing uses exactly the partial row shown by the renderer.
    pub(super) fn scrolled_y(&self, pane: PaneId, y: f32, height: f32) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        if let Some(scroll) = self.row_scroll(pane)
            && y >= scroll.first as f32 * height
        {
            return y + if y < scroll.end as f32 * height {
                scroll.fraction * height
            } else {
                height
            };
        }
        y
    }

    pub(super) fn stop_scrolling(&mut self) {
        let panes: Vec<_> = self.scrolling.keys().copied().collect();
        for pane in panes {
            self.scroll_extra(pane, false);
        }
        self.scrolling.clear();
    }

    pub(super) fn freeze_scrolling(&mut self) {
        for gesture in self.scrolling.values_mut() {
            gesture.motion.freeze();
        }
    }
}

fn consume_pixels(motion: &mut Motion, requested: f64, moved: f32) {
    if (requested - f64::from(moved)).abs() > 0.05 {
        *motion = Motion::default();
    } else {
        motion.consume(requested);
    }
}
