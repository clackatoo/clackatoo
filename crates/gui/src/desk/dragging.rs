//! Dragging files out of explorers with the mouse.
//!
//! A press on a file or a folder in an explorer is a click, as ever,
//! until the pointer goes half a cell from where it went down, held:
//! then it's dragged. While it is, the bar says what letting go there
//! would do, and the folder it would go into is lit. Letting go:
//!
//! - on an explorer, this one or another on the same machine, moves it
//!   into the folder there, or with Ctrl held copies it, as `x` or `y`
//!   then `p` would, so nothing is written over without asking: on a
//!   folder, into it; on a file, into the folder it's in; on `..`, into
//!   the folder above; on the heading or below the tree, into the root;
//! - on an editor, opens it there, as `:e` would;
//! - on a terminal, types its path at the cursor, quoted for the shell,
//!   with a space after, as file managers' drops do; on a chat, types it
//!   in the box, as it is.
//!
//! Each focuses the pane it's dropped on but a move or a copy, which
//! leaves the focus in the explorer it's dragged from, and the explorer
//! dropped on shows it where it went. Anywhere else, or after Esc, does
//! nothing. A path is its machine's, so it's never dropped on a pane of
//! another.

use clackatoo_protocol::{FileOp, PaneId};
use clackatoo_term::mouse::{MouseAction, MouseButton};
use winit::keyboard::{Key, NamedKey};

use super::Desk;
use super::view::Focus;
use crate::input::{KeyPress, Modifiers};

/// Something dragged from an explorer, or about to be once the pointer
/// goes far enough.
#[derive(Debug)]
pub(super) struct FileDrag {
    /// The explorer it's dragged from.
    from: PaneId,
    /// The path of what's dragged.
    path: String,
    /// Where the button went down.
    start: [f32; 2],
    /// Where the pointer is.
    at: [f32; 2],
    /// It's gone far enough from where the button went down to be a
    /// drag, not a click.
    moving: bool,
    /// The explorer lighting the folder it would go into.
    lit: Option<PaneId>,
}

/// What letting go of what's dragged would do.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Drop {
    /// Move it, or copy it, into the folder `dir` the explorer `pane`
    /// shows.
    Into { pane: PaneId, dir: String },
    /// Open it in the editor `pane`.
    Open(PaneId),
    /// Type its path in the terminal or the chat `pane`, quoted for the
    /// shell if `quoted`.
    Type { pane: PaneId, quoted: bool },
    /// Nothing, as this says why.
    No(String),
}

impl Desk {
    /// The button went down at `point` on row `row` of the explorer
    /// `pane`: what's on it is dragged if the pointer goes far enough
    /// before it comes up.
    pub(super) fn press_in_explorer(&mut self, pane: PaneId, point: [f32; 2], row: usize) {
        let path =
            (self.explorers.panes.get_mut(&pane)).and_then(|explorer| explorer.path_at_row(row));
        self.file_drag = path.map(|path| FileDrag {
            from: pane,
            path,
            start: point,
            at: point,
            moving: false,
            lit: None,
        });
    }

    /// Handles the mouse while something's pressed on in an explorer:
    /// motion drags it, once it's gone far enough, and letting go of the
    /// left button drops it. Returns whether the window must be redrawn,
    /// or `None` if nothing's dragged, or it's given up as another button
    /// goes down, for the mouse to be handled as ever.
    pub(super) fn file_drag_mouse(
        &mut self,
        point: [f32; 2],
        action: MouseAction,
        modifiers: Modifiers,
    ) -> Option<bool> {
        let drag = self.file_drag.as_mut()?;
        drag.at = point;
        match action {
            MouseAction::Motion(_) => {
                let cell = self.area.as_ref().map_or(8.0, |area| area.cell[1]);
                let gone = (point[0] - drag.start[0]).hypot(point[1] - drag.start[1]);
                if !drag.moving && gone < cell / 2.0 {
                    return Some(false);
                }
                drag.moving = true;
                Some(self.show_file_drop(modifiers.ctrl))
            }
            MouseAction::Release(MouseButton::Left) => {
                let moving = drag.moving;
                let drop = moving.then(|| self.file_drop_at(point));
                let drag = self.end_file_drag();
                if let (Some(drop), Some(drag)) = (drop, drag) {
                    self.drop_file(drag.from, drag.path, drop, modifiers.ctrl);
                }
                Some(moving)
            }
            MouseAction::Release(_) => Some(false),
            MouseAction::Press(_) => {
                self.end_file_drag();
                None
            }
        }
    }

    /// Shift, Ctrl or Alt went down or up while something's dragged: Ctrl
    /// says whether it's copied. Returns whether the window must be
    /// redrawn.
    pub(super) fn file_drag_modifiers(&mut self, modifiers: Modifiers) -> bool {
        self.file_drag.as_ref().is_some_and(|drag| drag.moving)
            && self.show_file_drop(modifiers.ctrl)
    }

    /// Esc while something's dragged lets go of it, dropping it nowhere.
    /// Returns whether it took the key.
    pub(super) fn file_drag_key(&mut self, press: &KeyPress<'_>) -> bool {
        let moving = self.file_drag.as_ref().is_some_and(|drag| drag.moving);
        if !moving || *press.key != Key::Named(NamedKey::Escape) {
            return false;
        }
        self.end_file_drag();
        true
    }

    /// Stops dragging, if anything was, putting out the folder lit and
    /// what the bar said of it. Returns what was dragged.
    pub(super) fn end_file_drag(&mut self) -> Option<FileDrag> {
        let drag = self.file_drag.take()?;
        for pane in [Some(drag.from), drag.lit].into_iter().flatten() {
            if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
                explorer.light_drop(None);
                explorer.show_drag(None);
            }
        }
        Some(drag)
    }

    /// Lights the folder what's dragged would go into, where the pointer
    /// is, and has the bar say what letting go would do: copy it, if
    /// `copy`. Returns whether the window must be redrawn.
    fn show_file_drop(&mut self, copy: bool) -> bool {
        let Some(drag) = &self.file_drag else {
            return false;
        };
        let (from, path, at, was_lit) = (drag.from, drag.path.clone(), drag.at, drag.lit);
        let drop = self.file_drop_at(at);
        let lit = match &drop {
            Drop::Into { pane, .. } => Some(*pane),
            _ => None,
        };
        let mut redraw = false;
        if was_lit != lit
            && let Some(explorer) = was_lit.and_then(|pane| self.explorers.panes.get_mut(&pane))
        {
            redraw |= explorer.light_drop(None);
        }
        let name = name_of(&path).to_owned();
        let label = match drop {
            Drop::Into { pane, dir } => {
                let explorer = self.explorers.panes.get_mut(&pane);
                let shown = explorer.map_or(dir.clone(), |explorer| {
                    redraw |= explorer.light_drop(Some(dir.clone()));
                    explorer.shown(&dir)
                });
                let (how, keys) = if copy {
                    ("copy", "Esc stops")
                } else {
                    ("move", "Ctrl copies, Esc stops")
                };
                let slash = if shown.ends_with('/') { "" } else { "/" };
                format!("{how} {name} into {shown}{slash}  ({keys})")
            }
            Drop::Open(_) => format!("open {name} here"),
            Drop::Type { .. } => format!("type {name}'s path here"),
            Drop::No(why) => why,
        };
        if let Some(drag) = &mut self.file_drag {
            drag.lit = lit;
        }
        if let Some(explorer) = self.explorers.panes.get_mut(&from) {
            redraw |= explorer.show_drag(Some(label));
        }
        redraw
    }

    /// What letting go of what's dragged at `point` would do.
    fn file_drop_at(&mut self, point: [f32; 2]) -> Drop {
        let Some(drag) = &self.file_drag else {
            return Drop::No(String::new());
        };
        let (from, path) = (drag.from, drag.path.clone());
        let Some(pane) = self
            .placement()
            .and_then(|placement| placement.pane_at(point))
        else {
            return Drop::No("drop it on a folder, an editor or a terminal".to_owned());
        };
        if self.pane_machine(pane) != self.pane_machine(from) {
            return Drop::No(format!("{} is on another machine", name_of(&path)));
        }
        if let Some(cell) = self.cell_of(point, pane)
            && let Ok(row) = usize::try_from(cell[0])
            && let Some(explorer) = self.explorers.panes.get_mut(&pane)
            && !self.editors.panes.contains_key(&pane)
        {
            let Some(dir) = explorer.folder_at_row(row) else {
                return Drop::No("that explorer is busy".to_owned());
            };
            let inside = dir
                .strip_prefix(path.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
            if inside {
                return Drop::No(format!("{} can't go into itself", name_of(&path)));
            }
            let fenced = [Some(from), Some(pane)]
                .into_iter()
                .flatten()
                .filter_map(|pane| self.explorers.panes.get(&pane))
                .any(|explorer| explorer.fenced_out(&dir));
            if fenced {
                return Drop::No(crate::explorer::FENCED.to_owned());
            }
            if parent_of(&path) == dir {
                return Drop::No(format!("{} is in that folder already", name_of(&path)));
            }
            return Drop::Into { pane, dir };
        }
        if self.takes_files(pane) {
            return Drop::Open(pane);
        }
        match self.pane_view(pane).map(super::view::PaneView::focus) {
            Some(Focus::Terminal(_)) => Drop::Type { pane, quoted: true },
            Some(Focus::Chat(_)) => Drop::Type {
                pane,
                quoted: false,
            },
            _ => Drop::No("drop it on a folder, an editor or a terminal".to_owned()),
        }
    }

    /// Does what letting go of `path`, dragged from the explorer `from`,
    /// does as `drop` says: copying it, not moving it, if `copy`.
    fn drop_file(&mut self, from: PaneId, path: String, drop: Drop, copy: bool) {
        match drop {
            Drop::Into { pane, dir } => {
                let to = join(&dir, name_of(&path));
                let op = if copy {
                    FileOp::Copy {
                        from: path,
                        to,
                        overwrite: None,
                        expect: None,
                    }
                } else {
                    FileOp::Move {
                        from: path,
                        to,
                        overwrite: None,
                        expect: None,
                    }
                };
                // Asked of the explorer dropped on, which shows where it
                // went, or asks before writing over what's there.
                self.file_op(pane, op);
            }
            Drop::Open(pane) => {
                self.show_pane(pane);
                if self.file_in(pane) != Some(path.as_str()) {
                    self.open_here(pane, path);
                }
            }
            Drop::Type { pane, quoted } => {
                let text = if quoted { shell_quoted(&path) } else { path };
                self.show_pane(pane);
                self.paste(&format!("{text} "));
            }
            Drop::No(why) => {
                if let Some(explorer) = self.explorers.panes.get_mut(&from)
                    && !why.is_empty()
                {
                    explorer.say(why);
                }
            }
        }
    }
}

/// The last part of `path`.
fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The folder `path` is in.
fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(slash) => &path[..slash],
        None => path,
    }
}

/// The path of `name` in the folder `dir`.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// `path` as a shell reads it as one word: as it is if nothing in it means
/// anything to a shell, or else in single quotes.
fn shell_quoted(path: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+,:@%=".contains(c);
    if !path.is_empty() && path.chars().all(plain) {
        return path.to_owned();
    }
    format!("'{}'", path.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::shell_quoted;

    #[test]
    fn a_path_is_quoted_only_if_a_shell_would_read_it_otherwise() {
        assert_eq!(shell_quoted("/p/src/main.rs"), "/p/src/main.rs");
        assert_eq!(shell_quoted("/p/my notes.txt"), "'/p/my notes.txt'");
        assert_eq!(shell_quoted("/p/it's"), r"'/p/it'\''s'");
        assert_eq!(shell_quoted("/p/$HOME*"), "'/p/$HOME*'");
    }
}
