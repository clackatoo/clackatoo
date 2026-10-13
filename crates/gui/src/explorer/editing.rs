//! Editing folders as text, as oil.nvim does: `e` in the explorer, or
//! `:Explore!`, shows the folder's listing in an editor in the explorer's
//! pane, an entry a line (see [`listing`]). Enter on a folder's line shows
//! its listing, and `-` the folder above's, so a line can be cut in one
//! and put in another. `:w` works out what the edits to every listing come
//! to, and the pane shows each step, asking whether to do them; `y` does
//! them, one at a time, through the daemon's file operations, which never
//! write over anything, and delete to the trash. The first that fails
//! stops the rest, and the pane says what was done and what wasn't.
//!
//! The explorer keeps what the listings were made of: each folder's
//! entries, as they were, and the entries their lines' marks name. The
//! desk keeps the editors, and their text, which is the window's alone: a
//! listing never reaches the daemon until it's done as file operations.
//!
//! A folder's listing is made from the explorer's listing of it, so a
//! folder to edit that isn't listed yet is expanded, or gone up to, and
//! shown once the daemon has listed it.

use clackatoo_protocol::{Attributes, Color, Entry, EntryKind, FileOpError, Style};
use clackatoo_term::RowBuilder;

use super::listing::{self, Folder, Original, Problem, Step};
use super::{Ask, Explorer, FENCED, Opening, join, parent_of, under};
use crate::colors;
use crate::editor::Stroke;

/// Folders' listings being edited as text, by the explorer's pane.
#[derive(Debug)]
pub(super) struct Editing {
    /// Each folder whose listing was made, in the order they were.
    folders: Vec<Edited>,
    /// The listing shown.
    shown: usize,
    /// The entries put in the listings, which their lines' marks name.
    originals: Vec<Original>,
    /// What the edits came to, once `:w` has asked, and how doing it goes.
    plan: Option<Plan>,
}

/// A folder whose listing was made.
#[derive(Debug)]
struct Edited {
    dir: String,
    /// What was in it when its listing was made, shown or not.
    entries: Vec<Entry>,
    /// The listing's text, as it was made.
    text: String,
}

/// What listings' edits come to, shown in the pane in place of the tree.
#[derive(Debug)]
struct Plan {
    steps: Vec<Step>,
    /// How many are done.
    done: usize,
    stage: Stage,
    /// The first step in view.
    top: usize,
    /// A folder edited has changed on disk since its listing was made.
    changed: bool,
}

/// How doing a plan goes.
#[derive(Debug, PartialEq, Eq)]
enum Stage {
    /// Asking whether to do it.
    Asking,
    /// Doing it: the step after those done is on its way, and the rest are
    /// stopped once it's done, if `stopping`.
    Doing { stopping: bool },
    /// Stopped: by the step after those done failing, as this says, or
    /// by Esc, or a lost connection.
    Stopped(String),
}

/// A folder's listing to show once it's listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Pending {
    /// The folder the cursor's in, once the path to reveal shows.
    Here,
    /// This folder, with the cursor on the line of the entry at `at`.
    Dir { dir: String, at: Option<String> },
    /// The root, once the daemon says what it is.
    NextRoot,
}

/// What `:w` in a listing comes to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Planned {
    /// Nothing has changed.
    Nothing,
    /// The steps are shown, asking whether to do them.
    Asking,
    /// It can't be done, as this says, on that line of that listing.
    Refused {
        at: Option<(usize, u64)>,
        why: String,
    },
}

impl Editing {
    /// Whether what the edits come to is shown, rather than their text.
    pub(super) fn planned(&self) -> bool {
        self.plan.is_some()
    }
}

impl Explorer {
    /// Whether folders' listings are being edited, in the pane or on
    /// their way there.
    pub(crate) fn editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Whether a listing's text is shown, in an editor, rather than what
    /// it comes to.
    pub(crate) fn editing_text(&self) -> bool {
        (self.editing.as_ref()).is_some_and(|editing| editing.plan.is_none())
    }

    /// `e`: shows the listing of the folder at the cursor, if it's open,
    /// or else the folder its line is in, as text, to edit.
    pub(super) fn edit_here(&mut self) {
        let dir = match self.lines.get(self.cursor) {
            Some(line) if line.entry().kind.expands() && self.asked.contains(&line.path()) => {
                line.path()
            }
            Some(line) => line.listing.dir.clone(),
            None => match &self.root {
                Some(root) => root.clone(),
                None => return,
            },
        };
        let at = self.lines.get(self.cursor).map(super::Line::path);
        self.edit_folder(dir, at.as_deref());
    }

    /// Shows the listing of a folder to edit, once it can: the root once
    /// the daemon says what it is, if `root`, or else the folder the
    /// cursor's in, once what's being revealed shows.
    pub(crate) fn edit_soon(&mut self, root: bool) {
        self.pending_edit = Some(if root {
            Pending::NextRoot
        } else {
            Pending::Here
        });
        self.edit_if_ready();
    }

    /// The folder listing `listing`, by its place, is of.
    pub(crate) fn folder_of(&self, listing: usize) -> Option<String> {
        let editing = self.editing.as_ref()?;
        Some(editing.folders.get(listing)?.dir.clone())
    }

    /// Back to the listings' text from what their edits come to.
    pub(crate) fn back_to_text(&mut self) {
        if let Some(editing) = &mut self.editing {
            editing.plan = None;
        }
    }

    /// Shows the listing waited for, if it can be now.
    pub(super) fn edit_if_ready(&mut self) {
        let Some(pending) = self.pending_edit.clone() else {
            return;
        };
        if self.root.is_none() {
            return;
        }
        match pending {
            Pending::Here if self.reveal.is_none() => {
                self.pending_edit = None;
                self.flatten_if_stale();
                self.edit_here();
            }
            Pending::Dir { dir, at } if self.listings.contains_key(&dir) => {
                self.pending_edit = None;
                self.edit_folder(dir, at.as_deref());
            }
            Pending::Here | Pending::Dir { .. } | Pending::NextRoot => {}
        }
    }

    /// The root is new: the listing waited for once it was is of it.
    pub(super) fn edit_rooted(&mut self) {
        if self.pending_edit == Some(Pending::NextRoot)
            && let Some(root) = self.root.clone()
        {
            self.pending_edit = Some(Pending::Dir {
                dir: root,
                at: None,
            });
        }
    }

    /// The folder `dir` can't be listed: a listing waited for of it
    /// isn't.
    pub(super) fn edit_unlisted(&mut self, dir: &str) {
        if matches!(&self.pending_edit, Some(Pending::Dir { dir: waited, .. }) if waited == dir) {
            self.pending_edit = None;
            self.say(format!("{dir} can't be listed"));
        }
    }

    /// Shows the listing of the folder `dir` to edit, with the cursor on
    /// the line of the entry at `at`, if it's there: made from the
    /// explorer's listing of it, which is asked for if there isn't one.
    pub(crate) fn edit_folder(&mut self, dir: String, at: Option<&str>) {
        if self.fenced_out(&dir) {
            return self.say(FENCED.to_owned());
        }
        let editing = self.editing.get_or_insert_with(|| Editing {
            folders: Vec::new(),
            shown: 0,
            originals: Vec::new(),
            plan: None,
        });
        let mark = |editing: &Editing| {
            let at = at?;
            let id = (editing.originals.iter()).position(|original| original.path == at)?;
            Some(listing::mark(id))
        };
        if let Some(index) = editing.folders.iter().position(|folder| folder.dir == dir) {
            editing.shown = index;
            let at = mark(editing);
            self.asks.push(Ask::Edit {
                listing: index,
                text: None,
                at,
            });
            return;
        }
        let Some(listing) = self.listings.get(&dir).cloned() else {
            // Listed once it's expanded, or the root.
            let root = self.root.clone().unwrap_or_default();
            if under(&dir, &root) {
                self.reveal = None;
                let mut above = dir.as_str();
                while above != root && under(above, &root) {
                    self.view.expanded.insert(above.to_owned());
                    above = parent_of(above);
                }
                self.sync();
            } else if under(&root, &dir) {
                self.root_at(dir.clone());
            } else {
                return self.say(format!("{dir} isn't in the explorer"));
            }
            if self
                .editing
                .as_ref()
                .is_some_and(|editing| editing.folders.is_empty())
            {
                self.editing = None;
            }
            self.pending_edit = Some(Pending::Dir {
                dir,
                at: at.map(str::to_owned),
            });
            return;
        };
        let editing = self.editing.as_mut().expect("made above");
        let mut text = String::new();
        for entry in &listing.entries {
            if !self.view.hidden && entry.name.starts_with('.') {
                continue;
            }
            let id = editing.originals.len();
            let path = join(&dir, &entry.name);
            text.push_str(&listing::line_of(id, &entry.name, entry.kind));
            text.push('\n');
            editing.originals.push(Original {
                path,
                kind: entry.kind,
            });
        }
        editing.shown = editing.folders.len();
        editing.folders.push(Edited {
            dir,
            entries: listing.entries.clone(),
            text: text.clone(),
        });
        let at = mark(editing);
        self.asks.push(Ask::Edit {
            listing: editing.shown,
            text: Some(text),
            at,
        });
    }

    /// The text listing `listing` was made with, as `:e!` puts it back.
    pub(crate) fn listing_as_made(&self, listing: usize) -> Option<&str> {
        let editing = self.editing.as_ref()?;
        Some(editing.folders.get(listing)?.text.as_str())
    }

    /// The listing shown, by its place.
    pub(crate) fn listing_shown(&self) -> Option<usize> {
        self.editing.as_ref().map(|editing| editing.shown)
    }

    /// The folder the listing shown is of.
    pub(crate) fn folder_shown(&self) -> Option<&str> {
        let editing = self.editing.as_ref()?;
        Some(editing.folders.get(editing.shown)?.dir.as_str())
    }

    /// Enter on `line`, a line of the listing shown: shows the listing of
    /// the folder it's of, or opens the file it's of.
    pub(crate) fn enter_line(&mut self, line: &str) {
        let Some(editing) = &self.editing else { return };
        let text = line.trim_start_matches([' ', '\t']);
        let mark = listing::mark_len(text);
        let original = (mark > 0)
            .then(|| listing::id_of(text))
            .flatten()
            .and_then(|id| editing.originals.get(id));
        match original {
            Some(original) if original.kind.expands() => {
                let dir = original.path.clone();
                self.edit_folder(dir, None);
            }
            Some(original) => {
                let path = original.path.clone();
                self.asks.push(Ask::Open {
                    path,
                    how: Opening::Here,
                });
            }
            None if text.trim_end().ends_with('/') => {
                self.say("that folder isn't made yet: :w makes it".to_owned());
            }
            None => self.say("that file isn't made yet: :w makes it".to_owned()),
        }
    }

    /// `-` in a listing: shows the listing of the folder above.
    pub(crate) fn edit_above(&mut self) {
        let Some(dir) = self.folder_shown().map(str::to_owned) else {
            return;
        };
        if dir == "/" {
            return;
        }
        let above = parent_of(&dir).to_owned();
        self.edit_folder(above, Some(&dir));
    }

    /// Stops editing listings, throwing away what's been edited.
    pub(crate) fn stop_editing(&mut self) {
        self.editing = None;
        self.pending_edit = None;
        self.stale = true;
    }

    /// `:w`: works out what the edits to the listings, whose texts are
    /// `texts`, by their places, come to, and if it's anything, shows it,
    /// asking whether to do it.
    pub(crate) fn plan(&mut self, texts: &[&str]) -> Planned {
        let Some(editing) = &mut self.editing else {
            return Planned::Nothing;
        };
        let folders: Vec<Folder<'_>> = (editing.folders.iter())
            .zip(texts)
            .map(|(folder, text)| Folder {
                dir: &folder.dir,
                entries: &folder.entries,
                text,
            })
            .collect();
        let steps = match listing::plan(&editing.originals, &folders) {
            Ok(steps) => steps,
            Err(Problem { at, why }) => return Planned::Refused { at, why },
        };
        if steps.is_empty() {
            return Planned::Nothing;
        }
        // A lesson's explorer touches nothing outside the lesson's folder.
        let fence = self.fence.as_deref();
        let outside =
            |path: &str| fence.is_some_and(|fence| !std::path::Path::new(path).starts_with(fence));
        let fenced = steps.iter().any(|step| match step {
            Step::Create { path, .. } | Step::Delete { path, .. } => outside(path),
            Step::Copy { from, to } | Step::Move { from, to, .. } => outside(from) || outside(to),
        });
        if fenced {
            return Planned::Refused {
                at: None,
                why: FENCED.to_owned(),
            };
        }
        let changed = (editing.folders.iter()).any(|folder| {
            (self.listings.get(&folder.dir)).is_some_and(|now| now.entries != folder.entries)
        });
        editing.plan = Some(Plan {
            steps,
            done: 0,
            stage: Stage::Asking,
            top: 0,
            changed,
        });
        Planned::Asking
    }

    /// A key while what the listings' edits come to is shown: `y` does it,
    /// `n` or Esc goes back to the text, and the motions scroll; while
    /// it's being done, Esc stops once the step on its way is; once it's
    /// stopped, any other key goes back to the tree.
    pub(super) fn plan_stroke(&mut self, stroke: Stroke) {
        let height = self.list_height();
        let Some(editing) = &mut self.editing else {
            return;
        };
        let Some(plan) = &mut editing.plan else {
            return;
        };
        let last = plan.steps.len().saturating_sub(1);
        let scrolled = match stroke {
            Stroke::Char {
                ch: 'j',
                ctrl: false,
            }
            | Stroke::Down => Some(plan.top.saturating_add(1)),
            Stroke::Char {
                ch: 'k',
                ctrl: false,
            }
            | Stroke::Up => Some(plan.top.saturating_sub(1)),
            Stroke::Char {
                ch: 'd',
                ctrl: true,
            }
            | Stroke::PageDown => Some(plan.top.saturating_add(height / 2)),
            Stroke::Char {
                ch: 'u',
                ctrl: true,
            }
            | Stroke::PageUp => Some(plan.top.saturating_sub(height / 2)),
            Stroke::Char {
                ch: 'G',
                ctrl: false,
            }
            | Stroke::End => Some(last),
            Stroke::Char {
                ch: 'g',
                ctrl: false,
            }
            | Stroke::Home => Some(0),
            _ => None,
        };
        if let Some(top) = scrolled {
            plan.top = top.min(last);
            return;
        }
        let yes = matches!(
            stroke,
            Stroke::Char {
                ch: 'y' | 'Y',
                ctrl: false
            }
        );
        let no = matches!(
            stroke,
            Stroke::Char {
                ch: 'n' | 'N' | 'q',
                ctrl: false
            } | Stroke::Escape
        );
        match plan.stage {
            Stage::Asking if yes => {
                plan.stage = Stage::Doing { stopping: false };
                let op = plan.steps[0].op();
                self.asks.push(Ask::Op(op));
            }
            Stage::Asking if no => {
                editing.plan = None;
                self.message = Some("nothing was done: back to the listing".to_owned());
                self.asks.push(Ask::BackToText);
            }
            Stage::Doing { .. } if stroke == Stroke::Escape => {
                plan.stage = Stage::Doing { stopping: true };
            }
            Stage::Stopped(_) => self.finish_editing(),
            Stage::Asking | Stage::Doing { .. } => {}
        }
    }

    /// Takes how a step of the plan being done went, `outcome`, and asks
    /// for the next, or stops. Returns whether one was being done.
    pub(super) fn step_done(&mut self, outcome: &Result<(), FileOpError>) -> bool {
        let why = match outcome {
            Ok(()) => None,
            Err(FileOpError::Exists { path, what, .. }) => {
                Some(format!("{} is already there, {what}", self.relative(path)))
            }
            Err(FileOpError::NoTrash(why)) => Some(format!(
                "{why}, so nothing was deleted: d in the tree deletes for good"
            )),
            Err(FileOpError::Failed(why)) => Some(why.clone()),
        };
        let Some(plan) = (self.editing.as_mut()).and_then(|editing| editing.plan.as_mut()) else {
            return false;
        };
        let Stage::Doing { stopping } = plan.stage else {
            return false;
        };
        match why.map_or(Ok(()), Err) {
            Ok(()) => {
                plan.done += 1;
                if plan.done == plan.steps.len() {
                    let count = plan.steps.len();
                    self.finish_editing();
                    self.message = Some(match count {
                        1 => "made the change".to_owned(),
                        count => format!("made all {count} changes"),
                    });
                    return true;
                }
                if stopping {
                    plan.stage = Stage::Stopped("stopped with Esc".to_owned());
                } else {
                    let op = plan.steps[plan.done].op();
                    self.asks.push(Ask::Op(op));
                }
            }
            Err(why) => plan.stage = Stage::Stopped(why),
        }
        true
    }

    /// The connection was lost while a plan was being done: whether the
    /// step on its way was done can't be known, so it stops, saying so.
    pub(super) fn plan_cut_off(&mut self) {
        if let Some(plan) = (self.editing.as_mut()).and_then(|editing| editing.plan.as_mut())
            && matches!(plan.stage, Stage::Doing { .. })
        {
            plan.stage = Stage::Stopped(
                "the connection was lost: the step on its way may or may not be done".to_owned(),
            );
        }
    }

    /// Done with the listings: back to the tree.
    fn finish_editing(&mut self) {
        self.stop_editing();
        self.asks.push(Ask::StopEditing);
    }

    /// What the bar says while what the listings' edits come to is shown.
    pub(super) fn plan_label(&self) -> Option<String> {
        let plan = self.editing.as_ref()?.plan.as_ref()?;
        let count = plan.steps.len();
        Some(match &plan.stage {
            Stage::Asking => match count {
                1 => "make this change? (y/n)".to_owned(),
                count => format!("make these {count} changes? (y/n)"),
            },
            Stage::Doing { stopping: false } => {
                format!(
                    "making change {} of {count}: Esc stops after it",
                    plan.done + 1
                )
            }
            Stage::Doing { stopping: true } => {
                format!("stopping after change {} of {count}", plan.done + 1)
            }
            Stage::Stopped(why) => format!(
                "stopped after {} of {count}: {why}. q goes back to the tree",
                plan.done
            ),
        })
    }

    /// Lays out what the listings' edits come to, a step a row, each
    /// marked as done, failed or on its way, below a heading.
    pub(super) fn settle_plan(&mut self) -> bool {
        let height = self.list_height();
        let Some(plan) = self
            .editing
            .as_ref()
            .and_then(|editing| editing.plan.as_ref())
        else {
            return false;
        };
        let mut rows = Vec::with_capacity(self.height);
        let mut header = RowBuilder::new(self.width);
        let heading = Style {
            attributes: Attributes::BOLD,
            ..colors::line_number(true)
        };
        let title = match &plan.stage {
            Stage::Asking if plan.changed => {
                "to do (a folder has changed since its listing was made)"
            }
            Stage::Asking => "to do",
            Stage::Doing { .. } => "doing",
            Stage::Stopped(_) => "stopped",
        };
        header.push(title, &heading, true);
        rows.push(header.finish(false));
        let failed = matches!(plan.stage, Stage::Stopped(_));
        let doing = matches!(plan.stage, Stage::Doing { .. });
        for (index, step) in plan.steps.iter().enumerate().skip(plan.top).take(height) {
            let mut builder = RowBuilder::new(self.width);
            let (mark, mark_style) = if index < plan.done {
                ("✓ ", colour(2))
            } else if index == plan.done && failed {
                ("✗ ", colour(1))
            } else if index == plan.done && doing {
                ("… ", colors::line_number(false))
            } else {
                ("  ", Style::DEFAULT)
            };
            builder.push(mark, &mark_style, true);
            let (verb, what, style) = self.described(step);
            builder.push(verb, &style, true);
            builder.push(" ", &Style::DEFAULT, true);
            builder.push(&what, &Style::DEFAULT, true);
            rows.push(builder.finish(false));
        }
        while rows.len() < self.height {
            rows.push(RowBuilder::new(self.width).finish(false));
        }
        rows.truncate(self.height);
        self.rows = rows;
        self.cursor_cell = None;
        self.name_cols.clear();
        true
    }

    /// How `step` is said: what's done, in its colour, and to what.
    fn described(&self, step: &Step) -> (&'static str, String, Style) {
        let shown = |path: &str| match self.relative(path) {
            "" => self.name.clone(),
            rest => rest.to_owned(),
        };
        let name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
        match step {
            Step::Create { path, dir: true } => ("make  ", format!("{}/", shown(path)), colour(2)),
            Step::Create { path, dir: false } => ("make  ", shown(path), colour(2)),
            Step::Copy { from, to } => (
                "copy  ",
                format!("{} to {}", shown(from), shown(to)),
                colour(6),
            ),
            Step::Move { from, to, aside } if parent_of(from) == parent_of(to) => {
                let out_of_the_way = if *aside { ", out of the way" } else { "" };
                let what = format!("{} to {}{out_of_the_way}", shown(from), name(to));
                ("rename", what, colour(3))
            }
            Step::Move { from, to, .. } => (
                "move  ",
                format!("{} to {}", shown(from), shown(to)),
                colour(3),
            ),
            Step::Delete { path, kind } => {
                let what = match kind {
                    EntryKind::Dir => " and everything in it",
                    EntryKind::Link { .. } => " (the link, not what it leads to)",
                    EntryKind::File { .. } | EntryKind::Other => "",
                };
                (
                    "delete",
                    format!("{}{what}, to the trash", shown(path)),
                    colour(1),
                )
            }
        }
    }
}

/// The palette's colour `index`, for a step's verb or mark.
fn colour(index: u8) -> Style {
    Style {
        fg: Color::Indexed(index),
        ..Style::DEFAULT
    }
}
