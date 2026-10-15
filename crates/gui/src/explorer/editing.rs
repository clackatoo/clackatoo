//! Editing folders as text, as oil.nvim does: `e` in the explorer, or
//! `:Explore!`, shows the folder's listing in an editor in the explorer's
//! pane, an entry a line (see [`listing`]). Enter on a folder's line shows
//! its listing, and `-` the folder above's, as Enter on the `..` line at
//! the top does, so a line can be cut in one and put in another. `:w`
//! works out what the edits to every listing come to, and the pane shows
//! each step, asking whether to do them; `y` does
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
//!
//! An editor's folder (see [`Explorer::of_folder`]) shows nothing else:
//! its folders are listed as they're asked for, wherever they are, its
//! root follows the folder shown, and once the edits are made, the folder
//! shown is listed afresh, rather than going back to a tree.

use std::collections::HashSet;

use clackatoo_protocol::{
    Color, Entry, EntryKind, Expect, FileOp, FileOpError, FilesAction, Style,
};
use clackatoo_term::RowBuilder;

use super::listing::{self, Folder, Original, Problem, Step};
use super::{Ask, Explorer, FENCED, Opening, join, parent_of, under};
use crate::colors;
use crate::editor::Stroke;

/// Folders' listings being edited as text, by the explorer's pane.
#[derive(Debug)]
pub(super) struct Editing {
    /// The session's number, picked at random, which its lines' marks
    /// name, so a line from another session's listing is told apart.
    session: u32,
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
    /// How many of its entries were left out, as no line can say their
    /// names.
    left_out: usize,
    /// It's no longer listed, nor watched: left as its listing was made,
    /// whose editor is gone, or gone from disk so. It's listed again if
    /// it's shown again.
    settled: bool,
    /// It's gone from disk, or can't be read, since its listing was made.
    /// It isn't asked for again, but until it settles, as it does if its
    /// listing has no changes, it's changed, and nothing is done.
    gone: bool,
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
    /// The number of the file operation on its way, for the step after
    /// those done, once it's been sent.
    waiting: Option<u64>,
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
    /// The root, once the daemon says what it is, with the cursor on the
    /// line of the entry at `at`.
    NextRoot { at: Option<String> },
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
    /// or else the folder its line is in, or the root, as on `..`, as
    /// text, to edit.
    pub(super) fn edit_here(&mut self) {
        let dir = match self.line() {
            Some(line) if line.entry().kind.expands() && self.asked.contains(&line.path()) => {
                line.path()
            }
            Some(line) => line.listing.dir.clone(),
            None => match &self.root {
                Some(root) => root.clone(),
                None => return,
            },
        };
        let at = self.line().map(super::Line::path);
        self.edit_folder(dir, at.as_deref());
    }

    /// Shows the listing of a folder to edit, once it can: the root once
    /// the daemon says what it is, if `root`, or else the folder the
    /// cursor's in, once what's being revealed shows.
    pub(crate) fn edit_soon(&mut self, root: bool) {
        self.pending_edit = Some(if root {
            Pending::NextRoot { at: None }
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
            Pending::Here | Pending::Dir { .. } | Pending::NextRoot { .. } => {}
        }
    }

    /// The root is new: the listing waited for once it was is of it.
    pub(super) fn edit_rooted(&mut self) {
        if let Some(Pending::NextRoot { at }) = &self.pending_edit
            && let Some(root) = self.root.clone()
        {
            let at = at.clone();
            self.pending_edit = Some(Pending::Dir { dir: root, at });
            // An editor's folder lists what it waits for.
            if self.folder {
                self.sync();
            }
        }
    }

    /// The folders an editor's folder has the daemon list: those whose
    /// listings are made, but for those settled, and the one waited for.
    /// Not its root, which is the folder shown, or one gone.
    pub(super) fn folders_wanted(&self) -> HashSet<String> {
        let made = (self.editing.iter()).flat_map(|editing| &editing.folders);
        let mut wanted: HashSet<String> = (made.filter(|folder| !folder.settled && !folder.gone))
            .map(|folder| folder.dir.clone())
            .collect();
        if let Some(Pending::Dir { dir, .. }) = &self.pending_edit {
            wanted.insert(dir.clone());
        }
        wanted
    }

    /// Whether it's waiting for a folder to be listed, to show its listing.
    pub(crate) fn waiting_for_listing(&self) -> bool {
        matches!(self.pending_edit, Some(Pending::Dir { .. }))
    }

    /// The folder `dir` can't be listed: a listing waited for of it
    /// isn't, and one made is gone, so it isn't asked for again and again.
    pub(super) fn edit_unlisted(&mut self, dir: &str) {
        if matches!(&self.pending_edit, Some(Pending::Dir { dir: waited, .. }) if waited == dir) {
            self.pending_edit = None;
            self.say(format!("{dir} can't be listed"));
        }
        let folders = (self.editing.iter_mut()).flat_map(|editing| &mut editing.folders);
        for folder in folders.filter(|folder| folder.dir == dir) {
            folder.gone = true;
        }
    }

    /// The listings, by their places, of folders gone from disk that
    /// haven't settled, for those with no changes to settle.
    pub(crate) fn gone_listings(&self) -> Vec<usize> {
        let folders = (self.editing.iter()).flat_map(|editing| &editing.folders);
        (folders.enumerate())
            .filter(|(_, folder)| folder.gone && !folder.settled)
            .map(|(place, _)| place)
            .collect()
    }

    /// Shows the listing of the folder `dir` afresh, with the cursor on the
    /// line of the entry at `at`, as its folder has changed on disk, or
    /// hidden files show or hide, or every listing's changes are thrown
    /// away: the listings made are forgotten, but not the session, so a
    /// line yanked from one still pastes, as a copy.
    pub(crate) fn list_afresh(&mut self, dir: String, at: Option<&str>) {
        if let Some(editing) = &mut self.editing {
            editing.folders.clear();
            editing.shown = 0;
            editing.plan = None;
            for original in &mut editing.originals {
                original.retired = true;
            }
        }
        self.pending_edit = None;
        self.edit_folder(dir, at);
        if self.folder {
            self.sync();
        }
    }

    /// Settles the folder listing `listing`, by its place, as it's left
    /// with its text as it was made: it's no longer listed, nor watched,
    /// until it's shown again. What its edits come to is worked out from
    /// its text as made.
    pub(crate) fn settle_listing(&mut self, listing: usize) {
        let Some(folder) =
            (self.editing.as_mut()).and_then(|editing| editing.folders.get_mut(listing))
        else {
            return;
        };
        folder.settled = true;
        folder.gone = false;
        if self.folder {
            self.sync();
        }
    }

    /// Asks the daemon to root an editor's folder at the folder it shows,
    /// or is about to, whatever it was last asked: another window may have
    /// rooted it elsewhere since.
    pub(crate) fn root_at_shown(&mut self) {
        if !self.folder {
            return;
        }
        let shown = match &self.pending_edit {
            Some(Pending::Dir { dir, .. }) => Some(dir.clone()),
            _ => self.folder_shown().map(str::to_owned),
        };
        if let Some(dir) = shown {
            self.outbox.push(FilesAction::Root(dir.clone()));
            self.asked_root = Some(dir);
        }
    }

    /// Shows the listing of the folder `dir` to edit, with the cursor on
    /// the line of the entry at `at`, if it's there, or on the way up if
    /// `at` is [`listing::UP`]: made from the
    /// explorer's listing of it, which is asked for if there isn't one.
    pub(crate) fn edit_folder(&mut self, dir: String, at: Option<&str>) {
        if self.fenced_out(&dir) {
            return self.say(FENCED.to_owned());
        }
        // An editor's folder is rooted at the folder it shows, for the
        // finder, and paths opened, to be from there: asked again only if
        // this window last asked for another, whatever's been answered.
        if self.folder && self.asked_root.as_ref().or(self.root.as_ref()) != Some(&dir) {
            self.outbox.push(FilesAction::Root(dir.clone()));
            self.asked_root = Some(dir.clone());
        }
        let editing = self.editing.get_or_insert_with(|| Editing {
            session: new_session(),
            folders: Vec::new(),
            shown: 0,
            originals: Vec::new(),
            plan: None,
        });
        let mark = |editing: &Editing| {
            let at = at?;
            // The way up has no mark: it's found by what it says.
            if at == listing::UP {
                return Some(listing::UP.to_owned());
            }
            let id = (editing.originals.iter())
                .position(|original| original.path == at && !original.retired)?;
            Some(listing::mark(editing.session, id))
        };
        if let Some(index) = editing.folders.iter().position(|folder| folder.dir == dir) {
            editing.shown = index;
            let at = mark(editing);
            // Shown again, a settled or gone folder is listed again.
            let folder = &mut editing.folders[index];
            let settled = std::mem::take(&mut folder.settled) | std::mem::take(&mut folder.gone);
            self.asks.push(Ask::Edit {
                listing: index,
                text: None,
                at,
            });
            if settled && self.folder {
                self.sync();
            }
            return;
        }
        let Some(listing) = self.listings.get(&dir).cloned() else {
            return self.list_to_edit(dir, at);
        };
        let up = self.goes_up_from(&dir);
        let editing = self.editing.as_mut().expect("made above");
        let mut text = String::new();
        // The way up first, which names nothing (see [`listing::is_up`]).
        if up {
            text.push_str(listing::UP);
            text.push('\n');
        }
        let mut left_out = 0;
        for entry in &listing.entries {
            if !self.view.hidden && entry.name.starts_with('.') {
                continue;
            }
            // A name no line can say is left out, and so left alone.
            if !says(entry, true) {
                left_out += 1;
                continue;
            }
            let path = join(&dir, &entry.name);
            // An entry as it was listed before keeps its number, so its
            // line, yanked then, is still its own.
            let before = (editing.originals.iter()).position(|original| {
                original.retired
                    && original.path == path
                    && original.kind == entry.kind
                    && original.stamp == entry.stamp
            });
            let id = before.unwrap_or(editing.originals.len());
            text.push_str(&listing::line_of(
                editing.session,
                id,
                &entry.name,
                entry.kind,
            ));
            text.push('\n');
            match before {
                Some(id) => editing.originals[id].retired = false,
                None => editing.originals.push(Original {
                    path,
                    kind: entry.kind,
                    stamp: entry.stamp,
                    retired: false,
                }),
            }
        }
        editing.shown = editing.folders.len();
        editing.folders.push(Edited {
            dir,
            entries: listing.entries.clone(),
            text: text.clone(),
            left_out,
            settled: false,
            gone: false,
        });
        if left_out > 0 {
            self.message = Some(left_out_said(left_out));
        }
        let at = mark(editing);
        self.asks.push(Ask::Edit {
            listing: editing.shown,
            text: Some(text),
            at,
        });
    }

    /// Has the folder `dir` listed, to show its listing to edit once it
    /// is, with the cursor on the line of the entry at `at`: asked for, by
    /// an editor's folder, or else expanded, or gone up to, as the root.
    fn list_to_edit(&mut self, dir: String, at: Option<&str>) {
        let pending = Pending::Dir {
            dir: dir.clone(),
            at: at.map(str::to_owned),
        };
        if self.folder {
            self.pending_edit = Some(pending);
            self.sync();
            return;
        }
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
            self.root_at(dir);
        } else {
            return self.say(format!("{dir} isn't in the explorer"));
        }
        if (self.editing.as_ref()).is_some_and(|editing| editing.folders.is_empty()) {
            self.editing = None;
        }
        self.pending_edit = Some(pending);
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
    /// the folder it's of, or opens the file it's of, or on the way up,
    /// shows the folder above's, as `-` does.
    pub(crate) fn enter_line(&mut self, line: &str) {
        let Some(editing) = &self.editing else { return };
        let text = line.trim_start_matches([' ', '\t']);
        let mark = listing::mark_len(text);
        let original = (mark > 0)
            .then(|| listing::id_of(editing.session, text))
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
            None if listing::is_up(text) => self.edit_above(),
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
        let steps = match listing::plan(editing.session, &editing.originals, &folders) {
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
            waiting: None,
        });
        Planned::Asking
    }

    /// Whether a folder being edited has changed since its listing was
    /// made, as its listing says now: its entries, or a file's stamp. One
    /// no longer listed may have.
    fn listings_changed(&self) -> bool {
        let Some(editing) = &self.editing else {
            return false;
        };
        // A settled folder isn't listed: what's done there expects what
        // was, one step at a time.
        (editing.folders.iter())
            .filter(|folder| !folder.settled)
            .any(|folder| {
                (self.listings.get(&folder.dir)).is_none_or(|now| now.entries != folder.entries)
            })
    }

    /// The file operation for step `index` of the plan, expecting what it
    /// starts from to be as it was listed: what was set aside, for a step
    /// that puts it back.
    fn step_op(&self, index: usize) -> Option<FileOp> {
        let editing = self.editing.as_ref()?;
        let plan = editing.plan.as_ref()?;
        let step = plan.steps.get(index)?;
        let mut source = step.source();
        if let Step::Move { from, .. } = step {
            let aside = (plan.steps[..index].iter()).find_map(|before| match before {
                Step::Move {
                    from: was,
                    to,
                    aside: true,
                } if to == from => Some(was.as_str()),
                _ => None,
            });
            source = aside.or(source);
        }
        let expect: Option<Expect> = source.and_then(|source| {
            (editing.originals.iter())
                .find(|original| original.path == source)
                .map(Original::expect)
        });
        Some(step.op(expect))
    }

    /// Step after those done has been sent, numbered `id`.
    pub(crate) fn step_sent(&mut self, id: u64) {
        if let Some(plan) = (self.editing.as_mut()).and_then(|editing| editing.plan.as_mut()) {
            plan.waiting = Some(id);
        }
    }

    /// Step after those done couldn't be sent, as `why` says.
    pub(crate) fn step_unsent(&mut self, why: String) {
        if let Some(plan) = (self.editing.as_mut()).and_then(|editing| editing.plan.as_mut())
            && matches!(plan.stage, Stage::Doing { .. })
        {
            plan.stage = Stage::Stopped(why);
        }
    }

    /// A key while what the listings' edits come to is shown: `y` does it,
    /// `n` or Esc goes back to the text, and the motions scroll; while
    /// it's being done, Esc stops once the step on its way is; once it's
    /// stopped, any other key goes back to the tree.
    pub(super) fn plan_stroke(&mut self, stroke: Stroke) {
        let height = self.list_height();
        let changed = self.listings_changed();
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
            // Nothing's done to a folder changed since its listing was
            // made: what the steps say may not be what's there.
            Stage::Asking if yes && changed => {
                editing.plan = None;
                self.message = Some(
                    "nothing was done: a folder changed since it was listed. :q! then e shows \
                     it afresh"
                        .to_owned(),
                );
                self.asks.push(Ask::BackToText);
            }
            Stage::Asking if yes => {
                plan.stage = Stage::Doing { stopping: false };
                if let Some(op) = self.step_op(0) {
                    self.asks.push(Ask::Step(op));
                }
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

    /// Takes how the file operation numbered `id` went, `outcome`, if
    /// it's the step of the plan on its way, and asks for the next, or
    /// stops. Returns whether it was.
    pub(super) fn step_done(&mut self, id: u64, outcome: &Result<(), FileOpError>) -> bool {
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
        if plan.waiting != Some(id) {
            return false;
        }
        plan.waiting = None;
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
                } else if let Some(op) = self.step_op(self.plan_done()) {
                    self.asks.push(Ask::Step(op));
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

    /// How many steps of the plan are done.
    fn plan_done(&self) -> usize {
        (self.editing.as_ref())
            .and_then(|editing| editing.plan.as_ref())
            .map_or(0, |plan| plan.done)
    }

    /// What was set aside by the steps done and not yet put back, as a
    /// stop leaves it: each thing's path, and where it is.
    fn left_aside(&self) -> Vec<(String, String)> {
        let Some(plan) = (self.editing.as_ref()).and_then(|editing| editing.plan.as_ref()) else {
            return Vec::new();
        };
        let done = &plan.steps[..plan.done];
        (done.iter())
            .filter_map(|step| match step {
                Step::Move {
                    from,
                    to,
                    aside: true,
                } => Some((from.clone(), to.clone())),
                _ => None,
            })
            .filter(|(_, to)| {
                !(done.iter()).any(|step| matches!(step, Step::Move { from, .. } if from == to))
            })
            .collect()
    }

    /// Done with the listings: back to the tree, or for an editor's
    /// folder, the folder shown listed afresh.
    fn finish_editing(&mut self) {
        let shown = (self.folder)
            .then(|| self.folder_shown().map(str::to_owned))
            .flatten();
        self.stop_editing();
        self.asks.push(Ask::StopEditing);
        if let Some(dir) = shown {
            self.asks.push(Ask::List { dir, at: None });
        }
    }

    /// The path of the entry `line`, a line of a listing, says, if its
    /// mark names one.
    pub(crate) fn path_of_line(&self, line: &str) -> Option<String> {
        let editing = self.editing.as_ref()?;
        let text = line.trim_start_matches([' ', '\t']);
        if listing::mark_len(text) == 0 {
            return None;
        }
        let id = listing::id_of(editing.session, text)?;
        Some(editing.originals.get(id)?.path.clone())
    }

    /// Whether a folder whose listing is made has changed since, as the
    /// daemon has listed it again: its entries, or a file's stamp.
    pub(crate) fn listings_stale(&self) -> bool {
        let Some(editing) = &self.editing else {
            return false;
        };
        (editing.folders.iter()).any(|folder| {
            (self.listings.get(&folder.dir)).is_some_and(|now| now.entries != folder.entries)
        })
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
            Stage::Stopped(why) => {
                let mut aside = String::new();
                for (from, to) in self.left_aside() {
                    let (from, to) = (self.relative(&from), self.relative(&to));
                    for part in [". ", from, " is set aside as ", to, ": put it back by hand"] {
                        aside.push_str(part);
                    }
                }
                format!(
                    "stopped after {} of {count}: {why}{aside}. q goes back to the tree",
                    plan.done
                )
            }
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
        let heading = colors::heading();
        let stage = match &plan.stage {
            Stage::Asking if plan.changed => {
                "to do (a folder has changed since its listing was made)"
            }
            Stage::Asking => "to do",
            Stage::Doing { .. } => "doing",
            Stage::Stopped(_) => "stopped",
        };
        let left_out: usize = (self.editing.iter())
            .flat_map(|editing| &editing.folders)
            .map(|folder| folder.left_out)
            .sum();
        let title = match left_out {
            0 => stage.to_owned(),
            1 => format!("{stage} (1 name left out)"),
            count => format!("{stage} ({count} names left out)"),
        };
        header.push(&title, &heading, true);
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

/// Whether a listing has a line for `entry`: not if it's hidden, unless
/// `hidden` files show, nor if its name is one no line can say, with a
/// line break, or a mark's characters, or bytes that aren't UTF-8, whose
/// path the window doesn't have.
fn says(entry: &Entry, hidden: bool) -> bool {
    let unsayable = |ch: char| ch == '\n' || ch == '\u{fffd}' || listing::in_marks(ch);
    (hidden || !entry.name.starts_with('.')) && !entry.name.chars().any(unsayable)
}

/// A number for a new editing session, picked at random, so two
/// sessions, in this window or another, all but never share one.
fn new_session() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    );
    // The hash's low half: as random as the rest.
    #[allow(clippy::cast_possible_truncation)]
    let session = hasher.finish() as u32;
    session
}

/// What's said of `count` entries left out of listings, as no line can
/// say their names.
fn left_out_said(count: usize) -> String {
    match count {
        1 => {
            "a name with a line break or a stray mark in it is left out, and left alone".to_owned()
        }
        count => {
            format!("{count} names with line breaks or stray marks are left out, untouched")
        }
    }
}
