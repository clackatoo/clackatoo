//! A file explorer pane: the tree of a project's files, kept by the
//! daemon a directory at a time (see `clackatoo_protocol`'s files), and
//! shown and moved about in here.
//!
//! What's expanded, where the cursor is, what's scrolled to and whether
//! hidden files show are the window's own. The daemon is asked to list a
//! directory as it's expanded, and sends it again whenever it changes,
//! until it's collapsed; a directory inside one collapsed stays expanded,
//! unseen and unlisted, for when its parent is expanded again.
//!
//! The tree is flattened into the lines it shows only when what's
//! expanded or listed changes, and only the lines in view are drawn, so a
//! directory of ten thousand files costs a frame what any other does.
//!
//! The keys are Vim's, as netrw's and nvim-tree's are:
//!
//! | Key | Does |
//! |---|---|
//! | `j` `k`, `gg` `G`, Ctrl+D Ctrl+U, Ctrl+F Ctrl+B | Moves, with a count |
//! | `l`, Enter | Opens a file; expands a directory (Enter collapses one too) |
//! | `h` | Collapses a directory, or goes to the one it's in |
//! | `-` | Goes up, making the directory above the root the root |
//! | `zo` `zc` `za` | Expands, collapses, toggles the directory at the cursor |
//! | `zR` `zM` | Expands every directory shown; collapses them all |
//! | `/` `?` `n` `N` | Searches the names shown, with the editor's patterns |
//! | `.`, `g.` | Shows hidden files, or hides them |
//! | `R` | Lists everything shown again |
//! | Ctrl+V Ctrl+S Ctrl+T | Opens a file in a split beside, below, or a tab |
//! | `a` | Makes a file where the cursor is, or a directory ending `/` |
//! | `r` | Renames, or moves, what's at the cursor |
//! | `d` | Deletes what's at the cursor, to the trash |
//! | `x` `y`, then `p` | Cuts or copies what's at the cursor, then moves or copies it where the cursor is |
//! | `e` | Edits the folder at the cursor as text (see [`editing`]) |
//! | `q` | Closes the explorer |

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use clackatoo_protocol::{
    Attributes, Color, Entry, EntryKind, FileOp, FileOpError, FilesAction, FilesId, FilesUpdate,
    GitMark, Side, Size, Style,
};
use clackatoo_term::{Row, RowBuilder};

use crate::colors;
use crate::editor::{Matching, Pattern, Stroke};
use crate::options::{Case, Options};
use crate::render::{CursorStyle, Highlight, HighlightKind, SceneCursor};

mod editing;
pub(crate) mod listing;
#[cfg(test)]
mod tests;

pub(crate) use editing::Planned;

/// What the explorer asks of the desk, which it can't do itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Open the file at this path, as `how` says.
    Open { path: String, how: Opening },
    /// Do this to files.
    Op(FileOp),
    /// Do this to files, as the next step of what listings' edits come
    /// to, telling the explorer the number it's sent with.
    Step(FileOp),
    /// Keep what's at this path to paste, to move it if `cut`, or copy it.
    Keep { path: String, cut: bool },
    /// Paste what's kept into this directory.
    Paste { dir: String },
    /// Close the explorer's pane.
    Close,
    /// Show folders' listing number `listing` in an editor in the pane,
    /// made with `text` if it's new, with the cursor on the line starting
    /// with the mark `at`, if one does.
    Edit {
        listing: usize,
        text: Option<String>,
        at: Option<String>,
    },
    /// Show the listing shown before what its edits come to was, again.
    BackToText,
    /// Stop editing listings, throwing away the editors.
    StopEditing,
}

/// Where a file the explorer opens goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opening {
    /// In the editor of the tab last focused, in place of its file, or in
    /// a new one beside the explorer if there's none.
    Here,
    /// In a new editor, split from that one, on this side.
    Split(Side),
    /// In a new tab.
    Tab,
}

/// What a window keeps of an explorer, to show it as it was when one
/// opens on the same root again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct View {
    /// The directories expanded.
    expanded: HashSet<String>,
    /// The path the cursor's on.
    cursor: Option<String>,
    /// Hidden files show.
    hidden: bool,
}

/// One directory, as the daemon last listed it.
#[derive(Debug)]
struct Listing {
    dir: String,
    entries: Vec<Entry>,
}

/// One line of the tree: an entry of a directory, so deep.
#[derive(Clone, Debug)]
struct Line {
    depth: u16,
    listing: Rc<Listing>,
    index: usize,
    /// It, or a directory it's in, is ignored.
    ignored: bool,
}

impl Line {
    fn entry(&self) -> &Entry {
        &self.listing.entries[self.index]
    }

    fn path(&self) -> String {
        join(&self.listing.dir, &self.entry().name)
    }

    /// Whether it's of the file at `path`.
    fn is(&self, path: &str) -> bool {
        let dir = self.listing.dir.as_str();
        let rest = match path.strip_prefix(dir) {
            Some(rest) if dir.ends_with('/') => Some(rest),
            Some(rest) => rest.strip_prefix('/'),
            None => None,
        };
        rest == Some(self.entry().name.as_str())
    }
}

/// What's being typed in the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Prompt {
    /// A search, backward if `backward`.
    Search { query: String, backward: bool },
    /// The name of something to make in `dir`.
    Add { dir: String, name: String },
    /// The new name of what's at `from`.
    Rename { from: String, name: String },
    /// Whether to do `op`, as `question` asks.
    Confirm { question: String, op: FileOp },
}

/// A pane showing a file explorer.
#[derive(Debug)]
pub(crate) struct Explorer {
    files: FilesId,
    /// The directory at its root, once the daemon has said, and as people
    /// write it.
    root: Option<String>,
    name: String,
    /// Each directory listed, by its path.
    listings: HashMap<String, Rc<Listing>>,
    view: View,
    /// The directories the daemon has been asked to list, and send as they
    /// change.
    asked: HashSet<String>,
    /// What git says of the files, by path.
    marks: BTreeMap<String, GitMark>,
    /// The tree, as shown, and whether it needs flattening again.
    lines: Vec<Line>,
    stale: bool,
    /// The line the cursor's on, and the first line in view.
    cursor: usize,
    top: usize,
    width: usize,
    height: usize,
    /// The rows drawn, as of the last settle, and where the cursor is in
    /// them.
    rows: Vec<Row>,
    cursor_cell: Option<(usize, usize)>,
    /// Where each line's name starts, in cells, as of the last settle.
    name_cols: Vec<usize>,
    /// A count typed, and `g` or `z`, waiting for the key after.
    count: Option<usize>,
    pending: Option<char>,
    prompt: Option<Prompt>,
    /// The last search, and whether it went backward.
    search: Option<(String, bool)>,
    case: Case,
    cursorline: bool,
    /// A path to put the cursor on once it shows, expanding what it's in.
    reveal: Option<String>,
    /// What to ask the daemon, oldest first.
    outbox: Vec<FilesAction>,
    /// What to ask the desk, oldest first.
    asks: Vec<Ask>,
    /// What the bar says until the next key, such as why a key did
    /// nothing.
    message: Option<String>,
    /// The directory it may not go above, nor change anything outside, as
    /// a lesson's explorer may not leave the lesson's scratch directory.
    fence: Option<String>,
    /// Folders' listings being edited as text, if they are, and one to
    /// show once it's listed.
    editing: Option<editing::Editing>,
    pending_edit: Option<editing::Pending>,
}

/// What an explorer says when it's asked to leave its fence.
pub(crate) const FENCED: &str = "a lesson's explorer stays in the lesson's folder";

impl Explorer {
    pub(crate) fn new(files: FilesId, options: &Options) -> Self {
        Self {
            files,
            root: None,
            name: String::new(),
            listings: HashMap::new(),
            view: View {
                hidden: options.explorerhidden,
                ..View::default()
            },
            asked: HashSet::new(),
            marks: BTreeMap::new(),
            lines: Vec::new(),
            stale: true,
            cursor: 0,
            top: 0,
            width: 0,
            height: 0,
            rows: Vec::new(),
            cursor_cell: None,
            name_cols: Vec::new(),
            count: None,
            pending: None,
            prompt: None,
            search: None,
            case: Case::of(options),
            cursorline: options.cursorline,
            reveal: None,
            outbox: Vec::new(),
            asks: Vec::new(),
            message: None,
            fence: None,
            editing: None,
            pending_edit: None,
        }
    }

    pub(crate) fn files(&self) -> FilesId {
        self.files
    }

    /// The directory at its root, once the daemon has said.
    pub(crate) fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }

    /// Whether the daemon has listed `path`, from the root, in a directory
    /// it's listing.
    pub(crate) fn lists(&self, path: &str) -> bool {
        let Some(root) = &self.root else {
            return false;
        };
        let (dir, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (join(root, dir), name),
            None => (root.clone(), path),
        };
        (self.listings.get(&dir))
            .is_some_and(|listing| listing.entries.iter().any(|entry| entry.name == name))
    }

    /// Takes in the options, as they've changed.
    pub(crate) fn set_options(&mut self, options: &Options) {
        self.case = Case::of(options);
        self.cursorline = options.cursorline;
    }

    pub(crate) fn resize(&mut self, size: Size) {
        self.height = usize::from(size.rows());
        self.width = usize::from(size.cols());
    }

    /// Its size, once it's been given one.
    pub(crate) fn size(&self) -> Option<Size> {
        let cols = u16::try_from(self.width).ok()?;
        let rows = u16::try_from(self.height).ok()?;
        Size::new(cols, rows).ok()
    }

    /// Takes what the bar is to say, if anything.
    pub(crate) fn take_message(&mut self) -> Option<String> {
        self.message.take()
    }

    /// What it keeps of how it's shown, for another explorer on its root.
    pub(crate) fn view(&self) -> View {
        let mut view = self.view.clone();
        view.cursor = self.lines.get(self.cursor).map(Line::path);
        view
    }

    /// Shows the tree as `view` says, as one on the same root was shown.
    pub(crate) fn restore(&mut self, view: View) {
        self.view = view;
        self.stale = true;
        self.sync();
        self.flatten_if_stale();
    }

    /// Makes `dir` the root: from the home directory if it starts `~/`, or
    /// else from the root if it's relative.
    pub(crate) fn root_at(&mut self, dir: String) {
        let dir = match &self.root {
            Some(root) if !dir.starts_with('/') && !dir.starts_with('~') => join(root, &dir),
            _ => dir,
        };
        if self.fenced_out(&dir) {
            self.say(FENCED.to_owned());
            return;
        }
        self.outbox.push(FilesAction::Root(dir));
    }

    /// Keeps it in `dir` from now on: it can't go above it, and nothing
    /// outside it can be made, moved, copied or deleted from it.
    pub(crate) fn fence(&mut self, dir: String) {
        self.fence = Some(dir);
    }

    /// Whether it's kept in a directory `path` isn't in.
    pub(crate) fn fenced_out(&self, path: &str) -> bool {
        (self.fence.as_deref()).is_some_and(|fence| !std::path::Path::new(path).starts_with(fence))
    }

    /// Whether it's kept in a directory, as a lesson's explorer is.
    pub(crate) fn fenced(&self) -> bool {
        self.fence.is_some()
    }

    /// Puts the cursor on the file at `path` once it shows, expanding the
    /// directories it's in.
    pub(crate) fn reveal(&mut self, path: String) {
        self.reveal = Some(path);
        self.expand_to_reveal();
        self.flatten_if_stale();
        self.edit_if_ready();
    }

    /// Whether a search, a name or an answer is being typed, in the bar.
    pub(crate) fn prompting(&self) -> bool {
        self.prompt.is_some()
    }

    /// Whether a key is half typed, as `g` or a count is, which the next
    /// key finishes.
    pub(crate) fn pending(&self) -> bool {
        self.count.is_some() || self.pending.is_some()
    }

    /// Says `message` in the bar until the next key.
    pub(crate) fn say(&mut self, message: String) {
        self.message = Some(message);
    }

    /// What the bar says: what's being typed, a message, or the root and
    /// the path at the cursor, from it.
    pub(crate) fn label(&self) -> String {
        if let Some(label) = self.plan_label() {
            return label;
        }
        match &self.prompt {
            Some(Prompt::Search { query, backward }) => {
                return format!("{}{query}▏", if *backward { '?' } else { '/' });
            }
            Some(Prompt::Add { dir, name }) => {
                return format!(
                    "new file in {}/ (end with / for a directory): {name}▏",
                    self.shown(dir)
                );
            }
            Some(Prompt::Rename { from, name }) => {
                return format!("rename {} to: {name}▏", self.shown(from));
            }
            Some(Prompt::Confirm { question, .. }) => return question.clone(),
            None => {}
        }
        if let Some(message) = &self.message {
            return message.clone();
        }
        match self.lines.get(self.cursor) {
            Some(line) => format!("{}  {}", self.name, self.relative(&line.path())),
            None => self.name.clone(),
        }
    }

    /// What the window's title says: the root.
    pub(crate) fn title(&self) -> &str {
        &self.name
    }

    /// `path`, from the root, or whole if it isn't under it.
    fn relative<'a>(&self, path: &'a str) -> &'a str {
        let Some(root) = &self.root else { return path };
        match path.strip_prefix(root.as_str()) {
            Some(rest) => rest.trim_start_matches('/'),
            None => path,
        }
    }

    /// `path` as the bar writes it: from the root's name.
    fn shown(&self, path: &str) -> String {
        match self.relative(path) {
            "" => self.name.clone(),
            rest if rest != path => format!("{}/{rest}", self.name),
            _ => path.to_owned(),
        }
    }

    // --- The daemon's news --------------------------------------------------

    /// Takes in what the daemon says about the explorer.
    pub(crate) fn update(&mut self, update: FilesUpdate) {
        self.heard(update);
        self.flatten_if_stale();
        self.edit_if_ready();
    }

    /// Takes in `update`, leaving the tree to be flattened.
    fn heard(&mut self, update: FilesUpdate) {
        match update {
            FilesUpdate::Root { path, name } => {
                self.root = Some(path);
                self.name = name;
                self.stale = true;
                self.sync();
                self.expand_to_reveal();
                self.edit_rooted();
            }
            FilesUpdate::Listed { dir, entries, .. } => {
                if !self.asked.contains(&dir) {
                    return;
                }
                let listing = Rc::new(Listing {
                    dir: dir.clone(),
                    entries,
                });
                self.listings.insert(dir, listing);
                self.stale = true;
            }
            FilesUpdate::Unlisted { dir, reason } => {
                self.edit_unlisted(&dir);
                self.listings.remove(&dir);
                self.asked.remove(&dir);
                self.view.expanded.remove(&dir);
                if self.root.as_ref() == Some(&dir) {
                    self.message = Some(format!("{}: {reason}", self.name));
                }
                // What was expanded in it no longer is.
                self.sync();
            }
            FilesUpdate::Git(marks) => self.marks = marks.into_iter().collect(),
            FilesUpdate::NoGit(reason) => self.message = Some(reason),
        }
    }

    /// The connection is new: nothing is listed for it yet. What's shown
    /// stays until it is again.
    pub(crate) fn reconnected(&mut self) {
        self.asked.clear();
        self.plan_cut_off();
    }

    /// Takes what's to be asked of the daemon, oldest first.
    pub(crate) fn take_outbox(&mut self) -> Vec<FilesAction> {
        std::mem::take(&mut self.outbox)
    }

    /// Takes what's to be asked of the desk, oldest first.
    pub(crate) fn take_asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.asks)
    }

    /// Takes how `op`, which it asked for, went: says so, or asks whether
    /// to write over what's there, or to delete for good what can't go to
    /// the trash.
    pub(crate) fn answered(&mut self, id: u64, op: FileOp, outcome: Result<(), FileOpError>) {
        // A step of listings' edits goes on to the next, or stops: it
        // never asks to write over, or delete for good.
        if self.step_done(id, &outcome) {
            return;
        }
        match (outcome, op) {
            (Ok(()), FileOp::Create { path, .. }) => {
                self.message = Some(format!("made {}", self.shown(&path)));
                self.reveal(path);
            }
            (Ok(()), FileOp::Move { to, .. }) => {
                self.message = Some(format!("moved to {}", self.shown(&to)));
                self.reveal(to);
            }
            (Ok(()), FileOp::Copy { to, .. }) => {
                self.message = Some(format!("copied to {}", self.shown(&to)));
                self.reveal(to);
            }
            (Ok(()), FileOp::Delete { path, trash, .. }) => {
                let gone = if trash { "to the trash" } else { "for good" };
                self.message = Some(format!("deleted {} {gone}", self.shown(&path)));
            }
            (Err(FileOpError::Exists { path, what, stamp }), op) => {
                // Only the file described is written over: one changed
                // since is described again.
                let op = match op {
                    FileOp::Move {
                        from, to, expect, ..
                    } => Some(FileOp::Move {
                        from,
                        to,
                        overwrite: Some(stamp),
                        expect,
                    }),
                    FileOp::Copy {
                        from, to, expect, ..
                    } => Some(FileOp::Copy {
                        from,
                        to,
                        overwrite: Some(stamp),
                        expect,
                    }),
                    FileOp::Create { .. } | FileOp::Delete { .. } => None,
                };
                let shown = self.shown(&path);
                match op {
                    Some(op) => {
                        let question = format!(
                            "{shown} is already there, {what}: write over it, losing it? (y/n)"
                        );
                        self.prompt = Some(Prompt::Confirm { question, op });
                    }
                    None => self.message = Some(format!("{shown} is already there, {what}")),
                }
            }
            (Err(FileOpError::NoTrash(why)), FileOp::Delete { path, expect, .. }) => {
                let question = format!(
                    "{why}: delete {}{} for good? (y/n)",
                    self.shown(&path),
                    self.what_else(&path)
                );
                let op = FileOp::Delete {
                    path,
                    trash: false,
                    expect,
                };
                self.prompt = Some(Prompt::Confirm { question, op });
            }
            (Err(err), _) => self.message = Some(err.to_string()),
        }
    }

    // --- The tree -----------------------------------------------------------

    /// The directories the daemon should list: the root, and each one
    /// expanded inside one it lists, unless it's hidden and hidden files
    /// don't show.
    fn wanted(&self) -> HashSet<String> {
        let Some(root) = &self.root else {
            return HashSet::new();
        };
        let mut dirs: Vec<&String> = (self.view.expanded.iter())
            .filter(|dir| under(dir, root) && *dir != root)
            .filter(|dir| self.view.hidden || !name_of(dir).starts_with('.'))
            .collect();
        // Each before those inside it.
        dirs.sort_by_key(|dir| dir.len());
        let mut wanted = HashSet::from([root.clone()]);
        for dir in dirs {
            if wanted.contains(parent_of(dir)) {
                wanted.insert(dir.clone());
            }
        }
        wanted
    }

    /// Asks the daemon to list what's newly wanted, and no longer to send
    /// what isn't.
    fn sync(&mut self) {
        let wanted = self.wanted();
        let mut collapse: Vec<&String> = self.asked.difference(&wanted).collect();
        collapse.sort();
        let mut expand: Vec<&String> = wanted.difference(&self.asked).collect();
        expand.sort();
        for dir in collapse {
            self.outbox.push(FilesAction::Collapse(dir.clone()));
        }
        for dir in expand {
            self.outbox.push(FilesAction::Expand(dir.clone()));
        }
        // What isn't wanted is forgotten: it's listed afresh if it is
        // again.
        self.listings.retain(|dir, _| wanted.contains(dir));
        self.asked = wanted;
        self.stale = true;
    }

    /// Flattens the tree, if what's expanded or listed has changed since
    /// it last was.
    fn flatten_if_stale(&mut self) {
        if self.stale {
            self.flatten();
        }
    }

    /// Flattens the tree into the lines shown, keeping the cursor on the
    /// path it was on, and putting it on the path to reveal if that
    /// shows now.
    fn flatten(&mut self) {
        let at = self.lines.get(self.cursor).map(Line::path);
        self.lines.clear();
        let Some(root) = &self.root else { return };
        // Each directory's listing, where it's got to, how deep it is, and
        // whether it's ignored.
        let mut stack: Vec<(Rc<Listing>, usize, u16, bool)> = Vec::new();
        if let Some(listing) = self.listings.get(root) {
            stack.push((Rc::clone(listing), 0, 0, false));
        }
        while let Some((listing, index, depth, in_ignored)) = stack.pop() {
            let Some(entry) = listing.entries.get(index) else {
                continue;
            };
            // The rest of its directory comes after it, and what's in it
            // comes between, pushed later.
            stack.push((Rc::clone(&listing), index + 1, depth, in_ignored));
            if !self.view.hidden && entry.name.starts_with('.') {
                continue;
            }
            let ignored = in_ignored || entry.ignored;
            if entry.kind.expands() {
                let path = join(&listing.dir, &entry.name);
                if self.asked.contains(&path)
                    && let Some(inside) = self.listings.get(&path)
                {
                    stack.push((Rc::clone(inside), 0, depth + 1, ignored));
                }
            }
            self.lines.push(Line {
                depth,
                listing: Rc::clone(&listing),
                index,
                ignored,
            });
        }
        self.stale = false;
        // The path to reveal, if it shows now, or else where a view kept
        // put it, or else the path the cursor was on. A path gone leaves
        // the cursor on the line it was on.
        let wanted = [self.reveal.clone(), self.view.cursor.clone(), at];
        for (which, wanted) in wanted.into_iter().enumerate() {
            let Some(wanted) = wanted else { continue };
            let Some(index) = self.lines.iter().position(|line| line.is(&wanted)) else {
                // Not there, though what it's in is listed and shown:
                // hidden, or gone, so it's forgotten.
                let dir = parent_of(&wanted);
                let shown =
                    self.root.as_deref() == Some(dir) || self.lines.iter().any(|line| line.is(dir));
                if which < 2 && shown && self.listings.contains_key(dir) {
                    if which == 0 {
                        self.reveal = None;
                    } else {
                        self.view.cursor = None;
                    }
                }
                continue;
            };
            self.cursor = index;
            match which {
                0 => {
                    self.reveal = None;
                    self.top = self.cursor.saturating_sub(self.list_height() / 2);
                }
                1 => self.view.cursor = None,
                _ => {}
            }
            break;
        }
        self.cursor = self.cursor.min(self.lines.len().saturating_sub(1));
    }

    /// Expands the directories the path to reveal is in, as far as the
    /// root.
    fn expand_to_reveal(&mut self) {
        let (Some(root), Some(path)) = (&self.root, &self.reveal) else {
            return;
        };
        if !under(path, root) || path == root {
            self.reveal = None;
            return;
        }
        let mut dir = parent_of(path);
        while dir != root && under(dir, root) {
            self.view.expanded.insert(dir.to_owned());
            dir = parent_of(dir);
        }
        self.sync();
    }

    /// Expands the directory at `path`.
    fn expand(&mut self, path: String) {
        self.view.expanded.insert(path);
        self.sync();
    }

    /// Collapses the directory at `path`. Those expanded inside it stay
    /// so, unseen.
    fn collapse(&mut self, path: &str) {
        self.view.expanded.remove(path);
        self.sync();
    }

    /// The lines shown below the root's.
    fn list_height(&self) -> usize {
        self.height.saturating_sub(1).max(1)
    }

    // --- Keys ---------------------------------------------------------------

    /// Does what `stroke` does. Returns whether it should be drawn again.
    pub(crate) fn stroke(&mut self, stroke: Stroke) -> bool {
        self.flatten_if_stale();
        let redraw = self.typed(stroke);
        self.flatten_if_stale();
        redraw
    }

    /// Does what `stroke` does, leaving the tree to be flattened.
    fn typed(&mut self, stroke: Stroke) -> bool {
        self.message = None;
        // Where a view kept put the cursor is forgotten once it's moved.
        self.view.cursor = None;
        // What listings' edits come to, shown in place of the tree.
        if self.editing.as_ref().is_some_and(editing::Editing::planned) {
            self.plan_stroke(stroke);
            return true;
        }
        if self.prompt.is_some() {
            self.prompt_stroke(stroke);
            return true;
        }
        let count = self.count.take();
        let pending = self.pending.take();
        let Stroke::Char { ch, ctrl } = stroke else {
            return self.named(stroke, count);
        };
        if ctrl {
            return self.ctrl(ch, count);
        }
        match (pending, ch) {
            (None, '0'..='9') if ch != '0' || count.is_some() => {
                let digit = ch.to_digit(10).expect("a digit") as usize;
                let count = count.unwrap_or(0).saturating_mul(10).saturating_add(digit);
                self.count = Some(count.min(1_000_000));
                return false;
            }
            (Some('g'), 'g') => self.go_to(count.map_or(0, |count| count - 1)),
            (Some('g') | None, '.') => self.toggle_hidden(),
            (Some('z'), 'o') => {
                if let Some(path) = self.dir_at_cursor() {
                    self.expand(path);
                }
            }
            (Some('z'), 'c') => self.close_fold(),
            (Some('z'), 'a') => self.toggle_at_cursor(),
            (Some('z'), 'R') => self.expand_all_shown(),
            (Some('z'), 'M') => self.collapse_all(),
            (Some(_), _) => {}
            (None, 'g' | 'z') => {
                self.count = count;
                self.pending = Some(ch);
                return false;
            }
            (None, 'j') => self.move_by(count.unwrap_or(1), true),
            (None, 'k') => self.move_by(count.unwrap_or(1), false),
            (None, 'G') => self.go_to(count.map_or(usize::MAX, |count| count - 1)),
            (None, 'l') => self.open_or_enter(),
            (None, 'h') => self.close_or_leave(),
            (None, '-') => self.up(),
            (None, '/' | '?') => {
                self.prompt = Some(Prompt::Search {
                    query: String::new(),
                    backward: ch == '?',
                });
            }
            (None, 'n' | 'N') => self.search_again(ch == 'N', count.unwrap_or(1)),
            (None, 'R') => self.outbox.push(FilesAction::Refresh),
            (None, 'q') => self.asks.push(Ask::Close),
            (None, 'a') => self.ask_add(),
            (None, 'r') => self.ask_rename(),
            (None, 'd') => self.ask_delete(),
            (None, 'e') => self.edit_here(),
            (None, 'x' | 'y') => {
                if let Some(line) = self.lines.get(self.cursor) {
                    let path = line.path();
                    let cut = ch == 'x';
                    let what = if cut { "cut" } else { "copied" };
                    self.message = Some(format!("{what} {}: p puts it", self.shown(&path)));
                    self.asks.push(Ask::Keep { path, cut });
                }
            }
            (None, 'p') => {
                if let Some(dir) = self.dir_here() {
                    self.asks.push(Ask::Paste { dir });
                }
            }
            _ => return false,
        }
        true
    }

    /// Does what a key that types no character does.
    fn named(&mut self, stroke: Stroke, count: Option<usize>) -> bool {
        match stroke {
            Stroke::Down => self.move_by(count.unwrap_or(1), true),
            Stroke::Up => self.move_by(count.unwrap_or(1), false),
            Stroke::Enter => match self.lines.get(self.cursor) {
                Some(line) if line.entry().kind.expands() => self.toggle_at_cursor(),
                Some(_) => self.open(Opening::Here),
                None => {}
            },
            Stroke::Right => self.open_or_enter(),
            Stroke::Left => self.close_or_leave(),
            Stroke::Home => self.go_to(0),
            Stroke::End => self.go_to(usize::MAX),
            Stroke::PageDown => self.page(count.unwrap_or(1), true, false),
            Stroke::PageUp => self.page(count.unwrap_or(1), false, false),
            Stroke::Escape => {}
            _ => return false,
        }
        true
    }

    /// Does what a key with Ctrl does.
    fn ctrl(&mut self, ch: char, count: Option<usize>) -> bool {
        match ch {
            'd' => self.page(count.unwrap_or(1), true, true),
            'u' => self.page(count.unwrap_or(1), false, true),
            'f' => self.page(count.unwrap_or(1), true, false),
            'b' => self.page(count.unwrap_or(1), false, false),
            'n' => self.move_by(count.unwrap_or(1), true),
            'p' => self.move_by(count.unwrap_or(1), false),
            'v' => self.open(Opening::Split(Side::Right)),
            's' => self.open(Opening::Split(Side::Below)),
            't' => self.open(Opening::Tab),
            _ => return false,
        }
        true
    }

    /// Takes `stroke` for what's being typed in the bar.
    fn prompt_stroke(&mut self, stroke: Stroke) {
        if let Some(Prompt::Confirm { op, .. }) = &self.prompt {
            let yes = stroke == Stroke::char('y') || stroke == Stroke::char('Y');
            if yes {
                self.asks.push(Ask::Op(op.clone()));
            } else {
                self.message = Some("nothing was done".to_owned());
            }
            self.prompt = None;
            return;
        }
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let text = match prompt {
            Prompt::Search { query, .. } => query,
            Prompt::Add { name, .. } | Prompt::Rename { name, .. } => name,
            Prompt::Confirm { .. } => unreachable!("answered above"),
        };
        match stroke {
            Stroke::Escape => self.prompt = None,
            Stroke::Backspace if text.is_empty() => self.prompt = None,
            Stroke::Backspace => {
                text.pop();
            }
            Stroke::Char {
                ch: 'u',
                ctrl: true,
            } => text.clear(),
            Stroke::Char {
                ch: 'w',
                ctrl: true,
            } => {
                let kept = text.trim_end_matches(|ch: char| !ch.is_alphanumeric());
                let kept = kept.trim_end_matches(char::is_alphanumeric).len();
                text.truncate(kept);
            }
            Stroke::Char { ch, ctrl: false } => text.push(ch),
            Stroke::Enter => {
                let prompt = self.prompt.take().expect("a prompt");
                self.prompted(prompt);
            }
            _ => {}
        }
    }

    /// Does what was typed in the bar, now Enter has been pressed.
    fn prompted(&mut self, prompt: Prompt) {
        match prompt {
            Prompt::Search { query, backward } => {
                let query = if query.is_empty() {
                    match &self.search {
                        Some((query, _)) => query.clone(),
                        None => return,
                    }
                } else {
                    query
                };
                self.search = Some((query, backward));
                self.search_again(false, 1);
            }
            Prompt::Add { dir, name } => {
                let name = name.trim();
                if name.is_empty() || name == "/" {
                    return;
                }
                let is_dir = name.ends_with('/');
                let path = join_relative(&dir, name.trim_end_matches('/'));
                self.asks
                    .push(Ask::Op(FileOp::Create { path, dir: is_dir }));
            }
            Prompt::Rename { from, name } => {
                let name = name.trim().trim_end_matches('/');
                if name.is_empty() {
                    return;
                }
                let to = join_relative(parent_of(&from), name);
                if to != from {
                    let op = FileOp::Move {
                        from,
                        to,
                        overwrite: None,
                        expect: None,
                    };
                    self.asks.push(Ask::Op(op));
                }
            }
            Prompt::Confirm { .. } => {}
        }
    }

    /// Moves the cursor `by` lines down, or up.
    fn move_by(&mut self, by: usize, down: bool) {
        let last = self.lines.len().saturating_sub(1);
        self.cursor = if down {
            self.cursor.saturating_add(by).min(last)
        } else {
            self.cursor.saturating_sub(by)
        };
    }

    /// Puts the cursor on line `line`, from 0, or the last.
    fn go_to(&mut self, line: usize) {
        self.cursor = line.min(self.lines.len().saturating_sub(1));
    }

    /// Scrolls `count` pages down, or up, or half pages if `half`, the
    /// cursor going with them.
    fn page(&mut self, count: usize, down: bool, half: bool) {
        let page = if half {
            (self.list_height() / 2).max(1)
        } else {
            self.list_height().saturating_sub(2).max(1)
        };
        let by = page.saturating_mul(count);
        let last = self.lines.len().saturating_sub(1);
        if down {
            self.top = self.top.saturating_add(by).min(last);
        } else {
            self.top = self.top.saturating_sub(by);
        }
        self.move_by(by, down);
    }

    /// The directory at the cursor, if it's on one.
    fn dir_at_cursor(&self) -> Option<String> {
        let line = self.lines.get(self.cursor)?;
        line.entry().kind.expands().then(|| line.path())
    }

    /// The directory things go in from the cursor: the one it's on, or
    /// else the one its line is in, or the root.
    fn dir_here(&self) -> Option<String> {
        match self.lines.get(self.cursor) {
            Some(line) if line.entry().kind.expands() => Some(line.path()),
            Some(line) => Some(line.listing.dir.clone()),
            None => self.root.clone(),
        }
    }

    /// `l`: opens the file at the cursor, or expands the directory, or
    /// goes into it if it's expanded.
    fn open_or_enter(&mut self) {
        let Some(line) = self.lines.get(self.cursor) else {
            return;
        };
        if !line.entry().kind.expands() {
            return self.open(Opening::Here);
        }
        let path = line.path();
        if !self.asked.contains(&path) {
            return self.expand(path);
        }
        let inside = (self.lines.get(self.cursor + 1)).is_some_and(|next| next.depth > line.depth);
        if inside {
            self.cursor += 1;
        }
    }

    /// `h`: collapses the directory at the cursor, or goes to the one its
    /// line is in.
    fn close_or_leave(&mut self) {
        let Some(line) = self.lines.get(self.cursor) else {
            return;
        };
        let path = line.path();
        if line.entry().kind.expands() && self.asked.contains(&path) {
            return self.collapse(&path);
        }
        self.go_to_parent();
    }

    /// Puts the cursor on the line of the directory its line is in, if
    /// that isn't the root.
    fn go_to_parent(&mut self) {
        let Some(line) = self.lines.get(self.cursor) else {
            return;
        };
        let depth = line.depth;
        if let Some(index) = (0..self.cursor)
            .rev()
            .find(|&index| self.lines[index].depth < depth)
        {
            self.cursor = index;
        }
    }

    /// `zc`: collapses the directory at the cursor, if it's expanded, or
    /// else the one its line is in, going to it.
    fn close_fold(&mut self) {
        if let Some(path) = self.dir_at_cursor()
            && self.asked.contains(&path)
        {
            return self.collapse(&path);
        }
        self.go_to_parent();
        if let Some(path) = self.dir_at_cursor() {
            self.collapse(&path);
        }
    }

    /// `za`: expands the directory at the cursor, or collapses it.
    fn toggle_at_cursor(&mut self) {
        let Some(path) = self.dir_at_cursor() else {
            return;
        };
        if self.asked.contains(&path) {
            self.collapse(&path);
        } else {
            self.expand(path);
        }
    }

    /// `zR`: expands every directory shown, a level at a time: never the
    /// whole tree at once.
    fn expand_all_shown(&mut self) {
        let dirs: Vec<String> = (self.lines.iter())
            .filter(|line| line.entry().kind.expands())
            .map(Line::path)
            .collect();
        self.view.expanded.extend(dirs);
        self.sync();
    }

    /// `zM`: collapses every directory, the cursor going to the one at the
    /// root its line is in.
    fn collapse_all(&mut self) {
        while self
            .lines
            .get(self.cursor)
            .is_some_and(|line| line.depth > 0)
        {
            self.go_to_parent();
        }
        self.view.expanded.clear();
        self.sync();
    }

    /// `.`: shows hidden files, or hides them.
    fn toggle_hidden(&mut self) {
        self.view.hidden = !self.view.hidden;
        self.message = Some(if self.view.hidden {
            "hidden files show".to_owned()
        } else {
            "hidden files are hidden".to_owned()
        });
        self.sync();
    }

    /// `-`: makes the directory above the root the root, keeping the old
    /// root expanded and the cursor on it.
    fn up(&mut self) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if root == "/" {
            return;
        }
        let parent = parent_of(&root).to_owned();
        if self.fenced_out(&parent) {
            return self.say(FENCED.to_owned());
        }
        self.view.expanded.insert(root.clone());
        self.reveal = Some(root);
        self.outbox.push(FilesAction::Root(parent));
    }

    /// Opens the file at the cursor, as `how` says.
    fn open(&mut self, how: Opening) {
        let Some(line) = self.lines.get(self.cursor) else {
            return;
        };
        if line.entry().kind.expands() {
            return self.toggle_at_cursor();
        }
        let path = line.path();
        self.asks.push(Ask::Open { path, how });
    }

    /// `a`: asks for the name of a file to make where the cursor is.
    fn ask_add(&mut self) {
        if let Some(dir) = self.dir_here() {
            self.prompt = Some(Prompt::Add {
                dir,
                name: String::new(),
            });
        }
    }

    /// `r`: asks for the new name of what's at the cursor.
    fn ask_rename(&mut self) {
        if let Some(line) = self.lines.get(self.cursor) {
            let name = line.entry().name.clone();
            self.prompt = Some(Prompt::Rename {
                from: line.path(),
                name,
            });
        }
    }

    /// `d`: asks whether to delete what's at the cursor, to the trash.
    fn ask_delete(&mut self) {
        if let Some(line) = self.lines.get(self.cursor) {
            let path = line.path();
            let what = self.what_else(&path);
            let question = format!("delete {}{what}, to the trash? (y/n)", self.shown(&path));
            let op = FileOp::Delete {
                path,
                trash: true,
                expect: None,
            };
            self.prompt = Some(Prompt::Confirm { question, op });
        }
    }

    /// What deleting the thing at `path` deletes besides: everything in
    /// it, if it's a directory, as listed. A link to one is deleted alone.
    fn what_else(&self, path: &str) -> &'static str {
        let dir = (self.listings.get(parent_of(path)))
            .and_then(|listing| {
                listing
                    .entries
                    .iter()
                    .find(|entry| entry.name == name_of(path))
            })
            .is_some_and(|entry| entry.kind == EntryKind::Dir);
        if dir { " and everything in it" } else { "" }
    }

    /// The pattern of the last search, if it's one the editor's patterns
    /// can match, saying why not if it isn't.
    fn pattern(&mut self) -> Option<Pattern> {
        let (query, _) = self.search.as_ref()?;
        match Pattern::new(query, Matching::Typed(self.case), "") {
            Ok(pattern) => Some(pattern),
            Err(why) => {
                self.message = Some(why);
                None
            }
        }
    }

    /// `n`: goes to the `count`th name shown the last search matches,
    /// after the cursor, or before it if it went backward, or the other
    /// way if `reverse`, round from the end to the start.
    fn search_again(&mut self, reverse: bool, count: usize) {
        let Some(pattern) = self.pattern() else {
            return;
        };
        let backward = self.search.as_ref().is_some_and(|(_, backward)| *backward) != reverse;
        let len = self.lines.len();
        if len == 0 {
            return;
        }
        let matches = |line: &Line| pattern.find_at(&line.entry().name, 0).is_some();
        let mut at = self.cursor;
        for _ in 0..count {
            let found = (1..=len)
                .map(|step| {
                    if backward {
                        (at + len - step % len) % len
                    } else {
                        (at + step) % len
                    }
                })
                .find(|&index| matches(&self.lines[index]));
            let Some(index) = found else {
                let query = self.search.as_ref().map_or("", |(query, _)| query.as_str());
                self.message = Some(format!("no name shown matches {query}"));
                return;
            };
            at = index;
        }
        self.cursor = at;
    }

    // --- The mouse ----------------------------------------------------------

    /// A click on row `row` of the pane, a double one if `double`: the
    /// cursor goes to the line there, which a double click opens, or
    /// expands or collapses, as Enter does. Returns whether it should be
    /// drawn again.
    pub(crate) fn click(&mut self, row: usize, double: bool) -> bool {
        self.flatten_if_stale();
        let Some(line) = (row.checked_sub(1)).map(|row| self.top + row) else {
            return false;
        };
        if line >= self.lines.len() || self.prompt.is_some() || self.editing.is_some() {
            return false;
        }
        self.cursor = line;
        self.view.cursor = None;
        if double {
            self.named(Stroke::Enter, None);
            self.flatten_if_stale();
        }
        true
    }

    /// Scrolls `lines` down, or up if it's negative, as the wheel does,
    /// the cursor staying in view.
    pub(crate) fn scroll(&mut self, lines: i64) -> bool {
        if self.editing.is_some() {
            return false;
        }
        self.flatten_if_stale();
        let by = usize::try_from(lines.unsigned_abs()).unwrap_or(usize::MAX);
        let last = self.lines.len().saturating_sub(1);
        let top = if lines < 0 {
            self.top.saturating_sub(by)
        } else {
            self.top.saturating_add(by).min(last)
        };
        if top == self.top {
            return false;
        }
        self.top = top;
        let bottom = top + self.list_height() - 1;
        self.cursor = self.cursor.clamp(top, bottom.max(top)).min(last);
        true
    }

    // --- Drawing ------------------------------------------------------------

    /// Lays out the rows shown, for [`scene`](Self::scene).
    pub(crate) fn settle(&mut self) {
        self.flatten_if_stale();
        if self.settle_plan() {
            return;
        }
        let height = self.list_height();
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + height {
            self.top = self.cursor + 1 - height;
        }
        self.top = self.top.min(self.lines.len().saturating_sub(1));
        let mut rows = Vec::with_capacity(self.height);
        let mut header = RowBuilder::new(self.width);
        let heading = Style {
            attributes: Attributes::BOLD,
            ..colors::line_number(true)
        };
        header.push(&self.name, &heading, true);
        rows.push(header.finish(false));
        self.name_cols.clear();
        self.cursor_cell = None;
        let end = (self.top + height).min(self.lines.len());
        for (index, line) in self.lines[self.top..end].iter().enumerate() {
            let (row, name_col) = self.draw_line(line);
            if self.top + index == self.cursor {
                self.cursor_cell = Some((index + 1, name_col));
            }
            self.name_cols.push(name_col);
            rows.push(row);
        }
        while rows.len() < self.height {
            rows.push(RowBuilder::new(self.width).finish(false));
        }
        rows.truncate(self.height);
        self.rows = rows;
    }

    /// Draws `line`: its git mark, the guides of the directories it's in,
    /// a chevron if it's a directory, and its name, coloured by what it
    /// is. Returns the row and where the name starts.
    fn draw_line(&self, line: &Line) -> (Row, usize) {
        let mut builder = RowBuilder::new(self.width);
        let entry = line.entry();
        let path = line.path();
        match self.mark_of(&path, entry.kind.expands()) {
            Some(mark) => {
                let (letter, style) = colors::git_mark(mark);
                builder.push(letter, &style, true);
                builder.push(" ", &Style::DEFAULT, true);
            }
            None => builder.push("  ", &Style::DEFAULT, true),
        }
        let guide = colors::line_number(false);
        for _ in 0..line.depth {
            builder.push("│ ", &guide, true);
        }
        let chevron = match entry.kind.expands() {
            true if self.asked.contains(&path) => "▾ ",
            true => "▸ ",
            false => "  ",
        };
        builder.push(chevron, &guide, true);
        let name_col = builder.filled();
        let mut style = name_style(entry.kind);
        if line.ignored {
            style.attributes |= Attributes::DIM;
        }
        builder.push(&entry.name, &style, true);
        (builder.finish(false), name_col)
    }

    /// What git says of the file at `path`, or of what's in it, rolled up,
    /// if it's a directory `dir`: the mark of what matters most in it.
    fn mark_of(&self, path: &str, dir: bool) -> Option<GitMark> {
        if let Some(&mark) = self.marks.get(path) {
            return Some(mark);
        }
        // In a directory git says is untracked whole.
        let root = self.root.as_deref().unwrap_or("/");
        let mut above = parent_of(path);
        while under(above, root) && above != root {
            if self.marks.get(above) == Some(&GitMark::Untracked) {
                return Some(GitMark::Untracked);
            }
            above = parent_of(above);
        }
        if !dir {
            return None;
        }
        let prefix = format!("{path}/");
        (self.marks.range(prefix.clone()..))
            .take_while(|(inside, _)| inside.starts_with(&prefix))
            .map(|(_, &mark)| match mark {
                GitMark::Conflicted => GitMark::Conflicted,
                GitMark::Untracked => GitMark::Untracked,
                _ => GitMark::Modified,
            })
            .max_by_key(|mark| match mark {
                GitMark::Conflicted => 2,
                GitMark::Untracked => 0,
                _ => 1,
            })
    }

    /// The rows to draw, and the cursor.
    pub(crate) fn scene(&self) -> (Vec<&Row>, Option<SceneCursor>) {
        let cursor = self.cursor_cell.map(|(line, col)| SceneCursor {
            line,
            col,
            style: CursorStyle::Block,
        });
        (self.rows.iter().collect(), cursor)
    }

    /// What's highlighted in view: the cursor's line, and the names the
    /// last search matches.
    pub(crate) fn highlights(&self) -> Vec<Highlight> {
        let mut highlights = Vec::new();
        if self.cursorline
            && let Some((line, _)) = self.cursor_cell
        {
            highlights.push(Highlight {
                line,
                cols: 0..self.width,
                kind: HighlightKind::CursorLine,
            });
        }
        let pattern = (self.search.as_ref())
            .and_then(|(query, _)| Pattern::new(query, Matching::Typed(self.case), "").ok());
        if let Some(pattern) = pattern {
            let shown = self.lines.iter().skip(self.top).zip(&self.name_cols);
            for (index, (line, &col)) in shown.enumerate() {
                let name = &line.entry().name;
                let Some(found) = pattern
                    .find_at(name, 0)
                    .filter(|found| found.end > found.start)
                else {
                    continue;
                };
                let start = col + crate::render::mono_width(&name[..found.start]);
                let end = start + crate::render::mono_width(&name[found.start..found.end]);
                highlights.push(Highlight {
                    line: index + 1,
                    cols: start..end.min(self.width),
                    kind: HighlightKind::Match,
                });
            }
        }
        highlights
    }
}

/// How a name is coloured, by what it names, as `ls` colours them: the
/// palette's blue for directories, cyan for links, green for programs,
/// and yellow for pipes, sockets and devices.
fn name_style(kind: EntryKind) -> Style {
    let (fg, attributes) = match kind {
        EntryKind::Dir => (Color::Indexed(4), Attributes::BOLD),
        EntryKind::Link { .. } => (Color::Indexed(6), Attributes::empty()),
        EntryKind::File { executable: true } => (Color::Indexed(2), Attributes::empty()),
        EntryKind::File { executable: false } => (Color::Default, Attributes::empty()),
        EntryKind::Other => (Color::Indexed(3), Attributes::empty()),
    };
    Style {
        fg,
        attributes,
        ..Style::DEFAULT
    }
}

/// The path of `name` in the directory `dir`.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// `path`, typed in the bar, from `dir`: whole if it's absolute.
fn join_relative(dir: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        join(dir, path)
    }
}

/// The directory `path` is in: itself if it's the root of everything.
fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(slash) => &path[..slash],
        None => path,
    }
}

/// The last part of `path`.
fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether `path` is `root` or inside it.
fn under(path: &str, root: &str) -> bool {
    root == "/"
        || path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}
