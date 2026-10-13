//! The finder: a picker over the panes, as Telescope's and fzf's are, for
//! the files in a project, the lines in them a pattern matches, the files
//! open, and the places a language server's answer gives.
//!
//! The daemon owns the files, so it lists and greps them, and sends what it
//! finds a batch at a time (see `clackatoo_protocol::Found`). The finder
//! matches what's typed against what's come (see [`rank`]), except for a
//! grep, whose pattern goes to the daemon instead, and keeps what's picked
//! and the preview beside it. It does no I/O and draws nothing: the desk
//! talks to the daemon for it, and [`view`] lays it out.
//!
//! | Key | Does |
//! |---|---|
//! | Typing, Backspace | Changes what's typed |
//! | Ctrl+U, Ctrl+W | Deletes what's typed, or its last word |
//! | Down, Ctrl+N; Up, Ctrl+P | The next or previous thing |
//! | Page Down, Page Up | A page further or back |
//! | Enter | Opens it in place of the focused pane's file, as `:e` does |
//! | Ctrl+V, Ctrl+X | Opens it beside the focused pane, or below |
//! | Ctrl+T | Opens it in a new tab |
//! | Esc, Ctrl+C | Closes the finder |

pub(crate) mod rank;
pub(crate) mod view;

use clackatoo_protocol::{Done, FileLocation, Found, FoundBuffer, FoundLine, Side};
use winit::keyboard::{Key, NamedKey};

use crate::command::Place;
use crate::input::KeyPress;
use crate::options::Case;
use crate::render::Ink;
use rank::{Items, Ranking};
use view::{FinderView, PreviewView, Row};

/// What the finder lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The project's files.
    Files,
    /// The lines in them a pattern matches.
    Grep,
    /// The files open in buffers.
    Buffers,
    /// The places a language server's answer gave.
    Places,
    /// What's wrong in the project's files, as their language servers
    /// say.
    Diagnostics,
    /// The marks the focused editor's file can go to.
    Marks,
    /// The boards the workspace shown sees.
    Boards,
}

impl Kind {
    fn title(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Grep => "Grep",
            Self::Buffers => "Buffers",
            Self::Places => "Places",
            Self::Diagnostics => "Diagnostics",
            Self::Marks => "Marks",
            Self::Boards => "Boards",
        }
    }
}

/// How many lines a preview asks for: more than a window shows, so moving
/// along a file's places mostly needs no more.
pub(crate) const PREVIEW_LINES: u16 = 160;

/// Something to pick: its file, and the place in it, if it's a line.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    /// The file's path, from the root unless it's absolute.
    path: String,
    /// The line, and the column in bytes, it goes to.
    at: Option<(u64, usize)>,
    /// How much of its text is the path, which is drawn quieter.
    path_len: usize,
    /// Byte ranges of its text to pick out, beside what's typed matches:
    /// where a grep's pattern matched.
    matched: Vec<(usize, usize)>,
}

/// A file to open, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Opening {
    pub(crate) path: String,
    /// The line, and the column in bytes, for the cursor to go to.
    pub(crate) at: Option<(u64, usize)>,
    pub(crate) place: Place,
}

/// What a key did to the finder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing to show.
    Nothing,
    /// Something shown changed.
    Changed,
    /// What's typed changed, and a grep must search for it.
    Search,
    /// It's to close.
    Close,
    /// It was to open the thing picked, but nothing is: nothing matches.
    NothingPicked,
    /// It's to close, opening a file.
    Open(Opening),
}

/// A preview of the file picked.
#[derive(Debug, Default)]
struct Preview {
    /// The daemon's search for a preview, while it goes on, and the file
    /// and line it's of.
    asked: Option<(u64, String, u64)>,
    shown: Option<Shown>,
}

/// A preview come from the daemon.
#[derive(Debug)]
struct Shown {
    /// The file, and the line asked about.
    path: String,
    line: u64,
    /// The first line's number, and the lines; or why there are none.
    lines: Result<(u64, Vec<String>), String>,
}

/// The finder's state.
#[derive(Debug)]
pub(crate) struct Finder {
    kind: Kind,
    query: String,
    /// The daemon's search filling the list, while it goes on.
    search: Option<u64>,
    /// How the last search finished, once it has.
    done: Option<Done>,
    /// Where it looks, made absolute, and as people write it.
    root: Option<(String, String)>,
    /// Each thing's text, which what's typed is matched against.
    items: Items,
    entries: Vec<Entry>,
    ranking: Ranking,
    /// The thing picked, by its rank.
    selected: usize,
    /// The first thing shown, by its rank.
    top: usize,
    /// How many things the list showed last, for paging.
    rows: usize,
    preview: Preview,
}

impl Finder {
    /// An empty finder of `kind`, starting with `query` typed, matching
    /// case as `case` says.
    pub(crate) fn new(kind: Kind, query: String, case: Case) -> Self {
        // Paths are better short; lines and places, in their order.
        let mut ranking = Ranking::new(case, matches!(kind, Kind::Files | Kind::Buffers));
        let items = Items::default();
        // A grep's query is the daemon's to look for.
        if kind != Kind::Grep {
            ranking.set_query(&query, &items);
        }
        Self {
            kind,
            query,
            search: None,
            done: None,
            root: None,
            items,
            entries: Vec::new(),
            ranking,
            selected: 0,
            top: 0,
            rows: 1,
            preview: Preview::default(),
        }
    }

    /// A finder of `kind` listing `found`, places given it about the file
    /// at `here`, shown from the deepest directory they and it share.
    pub(crate) fn places(kind: Kind, found: &[FileLocation], here: &str, case: Case) -> Self {
        let mut finder = Self::new(kind, String::new(), case);
        let mut root = here.rsplit_once('/').map_or("", |(dir, _)| dir);
        for place in found {
            while !place.path.starts_with(&format!("{root}/")) && !root.is_empty() {
                root = root.rsplit_once('/').map_or("", |(dir, _)| dir);
            }
        }
        let root = root.to_owned();
        for place in found {
            let path = match root.as_str() {
                "" => &place.path,
                root => (place.path.strip_prefix(root))
                    .and_then(|path| path.strip_prefix('/'))
                    .unwrap_or(&place.path),
            };
            finder.push_line(path, place.line, place.col, &place.text, &[]);
        }
        finder.ranking.add(&finder.items);
        finder.root = (!root.is_empty()).then(|| (root.clone(), root));
        finder.done = Some(Done::All);
        finder
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// What it lists.
    pub(crate) fn kind(&self) -> Kind {
        self.kind
    }

    /// The daemon's search filling the list, while it goes on.
    pub(crate) fn search(&self) -> Option<u64> {
        self.search
    }

    /// Starts the list again, from the daemon's search numbered `id`.
    pub(crate) fn searching(&mut self, id: u64) {
        self.search = Some(id);
        self.done = None;
        self.items.clear();
        self.entries.clear();
        self.ranking.clear();
        self.selected = 0;
        self.top = 0;
    }

    /// Ends the list where it is, with no search filling it, as for a grep
    /// of nothing.
    pub(crate) fn not_searching(&mut self) {
        self.searching(0);
        self.search = None;
        self.done = Some(Done::All);
    }

    /// Takes in some of what the daemon's search found.
    pub(crate) fn found(&mut self, found: Found) {
        match found {
            Found::Root { path, name } => self.root = Some((path, name)),
            Found::Files(files) => {
                for path in files {
                    self.items.push(&path);
                    self.entries.push(Entry {
                        path_len: path.len(),
                        path,
                        at: None,
                        matched: Vec::new(),
                    });
                }
            }
            Found::Lines(lines) => {
                for FoundLine {
                    path,
                    line,
                    col,
                    text,
                    matches,
                } in lines
                {
                    self.push_line(&path, line, col, &text, &matches);
                }
            }
            Found::Buffers(buffers) => {
                for FoundBuffer { path, modified } in buffers {
                    let text = if modified {
                        format!("{path} [+]")
                    } else {
                        path.clone()
                    };
                    self.items.push(&text);
                    self.entries.push(Entry {
                        path_len: path.len(),
                        path,
                        at: None,
                        matched: Vec::new(),
                    });
                }
            }
            // Each by its ID, which opening it gives back as its path.
            Found::Boards(boards) => {
                for board in boards {
                    let mut text = board.name.clone();
                    if !board.purpose.is_empty() {
                        text.push_str(" · ");
                        text.push_str(&board.purpose);
                    }
                    if board.archived {
                        text.push_str(" (archived)");
                    }
                    self.items.push(&text);
                    self.entries.push(Entry {
                        path: board.id.to_string(),
                        at: None,
                        path_len: 0,
                        matched: Vec::new(),
                    });
                }
            }
            Found::Preview { .. } => {}
            Found::Done(done) => {
                self.search = None;
                self.done = Some(done);
            }
        }
        self.ranking.add(&self.items);
    }

    /// Adds a line of a file, line `line` of `path`, whose text is `text`
    /// with `matched` picked out, going to byte `col` of it.
    fn push_line(
        &mut self,
        path: &str,
        line: u64,
        col: usize,
        text: &str,
        matched: &[(usize, usize)],
    ) {
        // Its indent left out, and tabs and the like as spaces: a byte for
        // a byte, so where matches are stays put.
        let indent = text.len() - text.trim_start().len();
        let text: String = (text[indent..].chars())
            .map(|ch| if ch.is_ascii_control() { ' ' } else { ch })
            .collect();
        let prefix = format!("{path}:{}: ", line + 1);
        let matched = (matched.iter())
            .filter(|&&(start, _)| start >= indent)
            .map(|&(start, end)| (start - indent + prefix.len(), end - indent + prefix.len()))
            .collect();
        self.items.push(&format!("{prefix}{text}"));
        self.entries.push(Entry {
            path: path.to_owned(),
            at: Some((line, col)),
            path_len: prefix.len(),
            matched,
        });
    }

    /// The path of the file `entry` is in, made absolute.
    fn absolute(&self, entry: &Entry) -> String {
        match &self.root {
            Some((root, _)) if !entry.path.starts_with('/') => format!("{root}/{}", entry.path),
            _ => entry.path.clone(),
        }
    }

    /// The thing picked, if anything matches.
    fn picked(&mut self) -> Option<&Entry> {
        let index = self.ranking.get(self.selected)?;
        self.entries.get(index)
    }

    /// Handles a key.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> Outcome {
        let modifiers = press.modifiers;
        // Not with Shift: Ctrl+Shift+C and the like are the terminal's.
        let ctrl = |press: &KeyPress<'_>| match press.key {
            Key::Character(text) if modifiers.ctrl && !modifiers.alt && !modifiers.shift => {
                text.chars().next().map(|ch| ch.to_ascii_lowercase())
            }
            _ => None,
        };
        match (press.key, ctrl(press)) {
            (Key::Named(NamedKey::Escape), _) | (_, Some('c')) => Outcome::Close,
            (Key::Named(NamedKey::Enter), _) => self.open(Place::Here),
            (_, Some('v')) => self.open(Place::Beside(Side::Right)),
            (_, Some('x')) => self.open(Place::Beside(Side::Below)),
            (_, Some('t')) => self.open(Place::Tab),
            (Key::Named(NamedKey::ArrowDown), _) | (_, Some('n')) => self.go(1),
            (Key::Named(NamedKey::ArrowUp), _) | (_, Some('p')) => self.go(-1),
            (Key::Named(NamedKey::PageDown), _) => self.go(self.page()),
            (Key::Named(NamedKey::PageUp), _) => self.go(-self.page()),
            (Key::Named(NamedKey::Backspace), _) => {
                if self.query.pop().is_none() {
                    return Outcome::Nothing;
                }
                self.typed()
            }
            (_, Some('u')) => {
                self.query.clear();
                self.typed()
            }
            (_, Some('w')) => {
                let kept = self
                    .query
                    .trim_end()
                    .rfind(' ')
                    .map_or(0, |space| space + 1);
                self.query.truncate(kept);
                self.typed()
            }
            _ if modifiers.ctrl || modifiers.alt => Outcome::Nothing,
            _ => match press.text {
                Some(text) => self.paste(text),
                None => Outcome::Nothing,
            },
        }
    }

    /// Types `text`.
    pub(crate) fn paste(&mut self, text: &str) -> Outcome {
        let before = self.query.len();
        (self.query).extend(text.chars().filter(|ch| !ch.is_control()));
        if self.query.len() == before {
            return Outcome::Nothing;
        }
        self.typed()
    }

    /// What's typed changed: the list is matched against it again, or for
    /// a grep, it's searched for.
    fn typed(&mut self) -> Outcome {
        self.selected = 0;
        self.top = 0;
        if self.kind == Kind::Grep {
            return Outcome::Search;
        }
        self.ranking.set_query(&self.query, &self.items);
        Outcome::Changed
    }

    /// Moves what's picked by `by` things.
    fn go(&mut self, by: isize) -> Outcome {
        let last = self.ranking.len().saturating_sub(1);
        let selected = self.selected.saturating_add_signed(by).min(last);
        if selected == self.selected {
            return Outcome::Nothing;
        }
        self.selected = selected;
        Outcome::Changed
    }

    /// Moves what's picked by `by` things, as the mouse's wheel does.
    pub(crate) fn scroll(&mut self, by: isize) -> Outcome {
        self.go(by)
    }

    /// Picks the thing shown in row `row` of the list, as a click does,
    /// and opens it in place if `open`, as a double click does.
    pub(crate) fn click(&mut self, row: usize, open: bool) -> Outcome {
        let rank = self.top + row;
        if rank >= self.ranking.len() {
            return Outcome::Nothing;
        }
        self.selected = rank;
        if open {
            return self.open(Place::Here);
        }
        Outcome::Changed
    }

    /// How many things a page is.
    fn page(&self) -> isize {
        isize::try_from(self.rows.max(1)).unwrap_or(1)
    }

    /// Opens the thing picked, at `place`.
    fn open(&mut self, place: Place) -> Outcome {
        let Some(entry) = self.picked().cloned() else {
            return Outcome::NothingPicked;
        };
        Outcome::Open(Opening {
            path: self.absolute(&entry),
            at: entry.at,
            place,
        })
    }

    /// The file and line the preview should show, for the thing picked, if
    /// what's shown or asked for doesn't already have it.
    pub(crate) fn preview_wanted(&mut self) -> Option<(String, u64)> {
        // A board is no file to preview.
        if self.kind == Kind::Boards {
            return None;
        }
        let entry = self.picked()?.clone();
        let (path, line) = (self.absolute(&entry), entry.at.map_or(0, |(line, _)| line));
        let has = |shown_path: &str, at: u64| shown_path == path && line.abs_diff(at) < 40;
        if let Some((_, asked_path, at)) = &self.preview.asked {
            return (!has(asked_path, *at)).then_some((path, line));
        }
        match &self.preview.shown {
            Some(shown) if has(&shown.path, shown.line) => None,
            _ => Some((path, line)),
        }
    }

    /// The daemon's search for a preview, while it goes on.
    pub(crate) fn preview_search(&self) -> Option<u64> {
        self.preview.asked.as_ref().map(|&(id, ..)| id)
    }

    /// The preview of line `line` of the file at `path` comes from the
    /// daemon's search numbered `id`.
    pub(crate) fn previewing(&mut self, id: u64, path: String, line: u64) {
        self.preview.asked = Some((id, path, line));
    }

    /// Takes in what the daemon's search for a preview found.
    pub(crate) fn preview_found(&mut self, found: Found) {
        let Some((_, path, line)) = &self.preview.asked else {
            return;
        };
        let lines = match found {
            Found::Preview { first, lines } => Ok((first, lines)),
            Found::Done(Done::Failed(why)) => Err(why),
            Found::Done(_) => {
                self.preview.asked = None;
                return;
            }
            _ => return,
        };
        self.preview.shown = Some(Shown {
            path: path.clone(),
            line: *line,
            lines,
        });
    }

    /// What to draw, for a list `rows` things tall and `cols` cells wide,
    /// and a preview of so many lines and cells, if there's room for one.
    pub(crate) fn view(
        &mut self,
        (rows, cols): (usize, usize),
        preview: Option<(usize, usize)>,
    ) -> FinderView {
        self.rows = rows;
        let count = self.ranking.len();
        self.selected = self.selected.min(count.saturating_sub(1));
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + rows {
            self.top = self.selected + 1 - rows;
        }
        let shown: Vec<Row> = (self.top..count.min(self.top + rows))
            .filter_map(|rank| self.row(rank, cols))
            .collect();
        let preview = preview.and_then(|size| self.preview_view(size));
        FinderView {
            title: self.kind.title(),
            place: self.root.as_ref().map(|(_, name)| name.clone()),
            query: format!("{}▏", self.query),
            count: self.count(),
            selected: (count > 0).then(|| self.selected - self.top),
            rows: shown,
            preview,
        }
    }

    /// How many things match, of how many, or why there are none.
    fn count(&self) -> String {
        let more = match (&self.done, self.search) {
            (Some(Done::Failed(why)), _) => return why.clone(),
            (Some(Done::CutShort), _) => "+",
            (_, Some(_)) => "…",
            _ => "",
        };
        format!("{} / {}{more}", self.ranking.len(), self.items.len())
    }

    /// The row for the thing ranked `rank`, `cols` cells wide.
    fn row(&mut self, rank: usize, cols: usize) -> Option<Row> {
        let index = self.ranking.get(rank)?;
        let entry = &self.entries[index];
        let text = self.items.get(index);
        let accents = self.ranking.matched_chars(text);
        // A file's directory quieter than its name; a line's file and
        // number quieter than its text.
        let quiet_to = if entry.at.is_some() {
            entry.path_len
        } else {
            text[..entry.path_len]
                .rfind('/')
                .map_or(0, |slash| slash + 1)
        };
        let modified_from = entry.path_len;
        let inks: Vec<Ink> = (text.char_indices().zip(0_u32..))
            .map(|((at, _), char_index)| {
                let picked = accents.binary_search(&char_index).is_ok()
                    || (entry.matched.iter()).any(|&(start, end)| (start..end).contains(&at));
                if picked {
                    Ink::Accent
                } else if at < quiet_to || (entry.at.is_none() && at >= modified_from) {
                    Ink::Muted
                } else {
                    Ink::Plain
                }
            })
            .collect();
        // A path too long keeps its end, where its name is; a line, its
        // start.
        Some(view::fit(text, &inks, cols, entry.at.is_none()))
    }

    /// The preview of the thing picked, `rows` lines tall and `cols` wide,
    /// if it's come.
    fn preview_view(&mut self, (rows, cols): (usize, usize)) -> Option<PreviewView> {
        let entry = self.picked()?.clone();
        let path = self.absolute(&entry);
        let shown = self.preview.shown.as_ref()?;
        if shown.path != path {
            return None;
        }
        let line = entry.at.map(|(line, _)| line);
        Some(match &shown.lines {
            Err(why) => PreviewView::note(why.clone()),
            Ok((first, lines)) => PreviewView::new(*first, lines, line, (rows, cols)),
        })
    }
}

#[cfg(test)]
mod tests;
