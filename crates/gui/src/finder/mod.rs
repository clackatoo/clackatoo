//! The finder: a picker over the panes, as Telescope's and fzf's are, for
//! the files in a project, the lines in them a pattern matches, the files
//! open, the places a language server's answer gives, and the symbols in a
//! file or a project; and for the tutor's lessons, and what's new, which
//! the window lists itself.
//!
//! The daemon owns the files, so it lists and greps them, and sends what it
//! finds a batch at a time (see `clackatoo_protocol::Found`). The finder
//! matches what's typed against what's come (see [`rank`]), except for a
//! grep, whose pattern goes to the daemon instead, and a search of the
//! project's symbols, which its language server matches, and where what's
//! typed only picks out the letters it matches; and keeps what's
//! picked and the preview beside it. It does no I/O and draws nothing: the
//! desk talks to the daemon for it, and [`view`] lays it out.
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

use std::fmt::Write as _;

use clackatoo_protocol::{
    Answer, Done, FileLocation, Found, FoundBuffer, FoundLine, FoundSymbol, Side, Span, Symbol,
};
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
    /// The tutor's lessons.
    Lessons,
    /// What's new, each line going to the lesson that teaches it.
    News,
    /// The symbols in the focused editor's file, as its language server
    /// lists them, in the order of its outline.
    Symbols,
    /// The symbols in the project whose names match what's typed, as the
    /// focused editor's language server finds them.
    ProjectSymbols,
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
            Self::Lessons => "Lessons",
            Self::News => "What's new",
            Self::Symbols => "Symbols",
            Self::ProjectSymbols => "Project symbols",
        }
    }

    /// Whether what's typed goes to the daemon to look for, rather than
    /// narrowing down what's come: a grep's pattern, or the name of a
    /// project's symbol, which its language server matches as it likes.
    fn searched(self) -> bool {
        matches!(self, Self::Grep | Self::ProjectSymbols)
    }

    /// Whether the window lists it itself, previews and all, with no help
    /// from the daemon.
    fn local(self) -> bool {
        matches!(self, Self::Lessons | Self::News)
    }
}

/// How many lines a preview asks for: more than a window shows, so moving
/// along a file's places mostly needs no more.
pub(crate) const PREVIEW_LINES: u16 = 160;

/// How many cells of what's typed the list quotes at most when nothing
/// matches it.
const QUOTED: usize = 40;

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
    /// For a line with `matched` in it, where its text after the path
    /// starts in the file's line, in bytes, so the line in the preview has
    /// the same picked out.
    in_line: usize,
    /// For a symbol, how deep it is in its file's outline, which it's
    /// indented by, and how much of its text is its name, which is drawn
    /// plainly and the rest quieter.
    symbol: Option<(u8, usize)>,
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
    /// The first line's number, the lines, and how they're coloured (see
    /// `Found::Preview`); or why there are none.
    lines: Result<(u64, Vec<String>, Vec<Span>), String>,
}

/// The finder's state.
#[derive(Debug)]
pub(crate) struct Finder {
    kind: Kind,
    query: String,
    /// The daemon's search filling the list, while it goes on.
    search: Option<u64>,
    /// The query to a file's language server whose answer fills the list,
    /// until it comes.
    asking: Option<u64>,
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
    /// The preview of each thing, by its index, for a list the window
    /// makes itself.
    local_previews: Vec<Vec<String>>,
}

/// Something for a list the window makes itself: what [`Opening::path`]
/// says when it's picked, its text, how much of the text is its name,
/// which is drawn plainly and the rest quieter, and its preview.
#[derive(Debug, Clone)]
pub(crate) struct LocalItem {
    pub(crate) name: String,
    pub(crate) text: String,
    pub(crate) name_len: usize,
    pub(crate) preview: Vec<String>,
}

impl Finder {
    /// An empty finder of `kind`, starting with `query` typed, matching
    /// case as `case` says.
    pub(crate) fn new(kind: Kind, query: String, case: Case) -> Self {
        // Paths are better short; lines and places, in their order.
        let mut ranking = Ranking::new(case, matches!(kind, Kind::Files | Kind::Buffers));
        // A project's symbols are matched by their language server: what's
        // typed only picks out the letters it matches, as in a list of
        // files. A grep's lines say where its pattern matched instead.
        if kind == Kind::ProjectSymbols {
            ranking.only_pick_out();
        }
        let items = Items::default();
        // A grep's query is the daemon's to look for.
        if !kind.searched() {
            ranking.set_query(&query, &items);
        }
        Self {
            kind,
            query,
            search: None,
            asking: None,
            done: None,
            root: None,
            items,
            entries: Vec::new(),
            ranking,
            selected: 0,
            top: 0,
            rows: 1,
            preview: Preview::default(),
            local_previews: Vec::new(),
        }
    }

    /// A finder of `kind` listing `items`, in their order, made by the
    /// window rather than found by the daemon.
    pub(crate) fn local(kind: Kind, items: Vec<LocalItem>, case: Case) -> Self {
        let mut finder = Self::new(kind, String::new(), case);
        for item in items {
            finder.items.push(&item.text);
            finder.entries.push(Entry {
                path: item.name,
                at: None,
                path_len: item.name_len,
                matched: Vec::new(),
                in_line: 0,
                symbol: None,
            });
            finder.local_previews.push(item.preview);
        }
        finder.ranking.add(&finder.items);
        finder.done = Some(Done::All);
        finder
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
        // What it searches for picks out the letters in what it finds.
        if self.kind == Kind::ProjectSymbols {
            self.ranking.set_query(&self.query, &self.items);
        }
    }

    /// The query to a file's language server whose answer fills the list,
    /// until it comes.
    pub(crate) fn asking(&self) -> Option<u64> {
        self.asking
    }

    /// Waits for the answer to the query numbered `id`, to a file's
    /// language server, to fill the list.
    pub(crate) fn ask(&mut self, id: u64) {
        self.asking = Some(id);
        self.done = None;
    }

    /// Takes in the answer to the query it waited for, the symbols in the
    /// file at `path`, made absolute: or why there are none. A list cut
    /// short says so, as a search's does.
    pub(crate) fn answered(&mut self, path: &str, answer: Answer) {
        self.asking = None;
        self.done = Some(match answer {
            Answer::Symbols { symbols, more } => {
                for symbol in &symbols {
                    self.push_symbol(path, symbol, false);
                }
                self.ranking.add(&self.items);
                if more { Done::CutShort } else { Done::All }
            }
            Answer::Failed(why) => Done::Failed(why),
            _ => Done::All,
        });
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
                        in_line: 0,
                        symbol: None,
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
                        in_line: 0,
                        symbol: None,
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
                        in_line: 0,
                        symbol: None,
                    });
                }
            }
            Found::Symbols(symbols) => {
                for FoundSymbol { path, symbol } in &symbols {
                    self.push_symbol(path, symbol, true);
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
        // The text may start some way into the line, but its first match
        // is at `col` in both.
        let in_line =
            (matched.first()).map_or(0, |&(first, _)| (col + indent).saturating_sub(first));
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
            in_line,
            symbol: None,
        });
    }

    /// Adds `symbol`, in the file at `path`: its name, then, quieter, what
    /// it is and what it's inside, and if `with_path`, its file and line.
    fn push_symbol(&mut self, path: &str, symbol: &Symbol, with_path: bool) {
        let mut text = symbol.name.clone();
        let name_len = text.len();
        text.push_str("  ");
        text.push_str(symbol_kind(symbol.kind));
        if !symbol.container.is_empty() {
            text.push_str(" · ");
            text.push_str(&symbol.container);
        }
        if with_path {
            let _ = write!(text, "  {path}:{}", symbol.line + 1);
        }
        self.items.push(&text);
        self.entries.push(Entry {
            path: path.to_owned(),
            at: Some((symbol.line, symbol.col)),
            path_len: 0,
            matched: Vec::new(),
            in_line: 0,
            // A project's are in no outline.
            symbol: Some((if with_path { 0 } else { symbol.depth }, name_len)),
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
        if self.kind.searched() {
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
        // A board is no file to preview, and a lesson's is its own.
        if self.kind == Kind::Boards || self.kind.local() {
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
            Found::Preview {
                first,
                lines,
                spans,
            } => Ok((first, lines, spans)),
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
            empty: self.nothing_matches(cols),
        }
    }

    /// What a list `cols` cells wide says when nothing matches what's
    /// typed, once it's sure: not while it's still looking, not when
    /// looking failed, which the count says, and not when it stopped short
    /// of all there was, where more might.
    fn nothing_matches(&self, cols: usize) -> Option<String> {
        let looking = self.search.or(self.asking).is_some();
        let unsure = matches!(self.done, Some(Done::Failed(_) | Done::CutShort));
        if self.query.is_empty() || self.ranking.len() > 0 || looking || unsure {
            return None;
        }
        // What's typed, cut short to the room the words leave.
        let words = crate::render::mono_width("Nothing matches ‘’");
        let room = cols.saturating_sub(words).min(QUOTED);
        Some(format!(
            "Nothing matches ‘{}’",
            view::cut(&self.query, room, false)
        ))
    }

    /// How many things match, of how many, or why there are none.
    fn count(&self) -> String {
        let more = match (&self.done, self.search.or(self.asking)) {
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
        let accents = match entry.symbol {
            // A project's symbols are found by their names alone.
            Some((_, name_len)) if self.kind == Kind::ProjectSymbols => {
                self.ranking.matched_chars(&text[..name_len])
            }
            _ => self.ranking.matched_chars(text),
        };
        if let Some((depth, name_len)) = entry.symbol {
            return Some(symbol_row(text, &accents, (depth, name_len), cols));
        }
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
        // How much what's wrong matters, coloured as in an editor.
        let severity = (self.kind == Kind::Diagnostics)
            .then(|| severity(&text[entry.path_len..]))
            .flatten()
            .map(|(len, ink)| (entry.path_len..entry.path_len + len, ink));
        let inks: Vec<Ink> = (text.char_indices().zip(0_u32..))
            .map(|((at, _), char_index)| {
                let picked = accents.binary_search(&char_index).is_ok()
                    || (entry.matched.iter()).any(|&(start, end)| (start..end).contains(&at));
                match &severity {
                    _ if picked => Ink::Accent,
                    Some((word, ink)) if word.contains(&at) => *ink,
                    _ if at < quiet_to || (entry.at.is_none() && at >= modified_from) => Ink::Muted,
                    _ => Ink::Plain,
                }
            })
            .collect();
        // A path too long keeps its end, where its name is; a line, or a
        // lesson, its start.
        Some(view::fit(
            text,
            &inks,
            cols,
            entry.at.is_none() && !self.kind.local(),
        ))
    }

    /// The preview of the thing picked, `rows` lines tall and `cols` wide,
    /// if it's come.
    fn preview_view(&mut self, (rows, cols): (usize, usize)) -> Option<PreviewView> {
        if self.kind.local() {
            let index = self.ranking.get(self.selected)?;
            let lines = self.local_previews.get(index)?;
            return Some(PreviewView::new(0, lines, None, (rows, cols)));
        }
        let entry = self.picked()?.clone();
        let path = self.absolute(&entry);
        let shown = self.preview.shown.as_ref()?;
        if shown.path != path {
            return None;
        }
        let line = entry.at.map(|(line, _)| line);
        Some(match &shown.lines {
            Err(why) => PreviewView::note(why.clone()),
            Ok((0, lines, _)) if lines.is_empty() => PreviewView::note("Empty file".into()),
            Ok((first, lines, spans)) => {
                // Where a grep matched, picked out in its line as in the
                // list.
                let in_line = |at: usize| at.saturating_sub(entry.path_len) + entry.in_line;
                let matched: Vec<(usize, usize)> = (entry.matched.iter())
                    .map(|&(start, end)| (in_line(start), in_line(end)))
                    .collect();
                PreviewView::coloured(*first, lines, line, (rows, cols), spans, &matched)
            }
        })
    }
}

/// How much what a diagnostic's `text` says is wrong matters, as the daemon
/// writes it, starting `error: ` or the like: how long that word is, and
/// its ink, as an editor colours it.
fn severity(text: &str) -> Option<(usize, Ink)> {
    use clackatoo_text::Severity;
    let (word, _) = text.split_once(": ")?;
    let severity = match word {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        "info" => Severity::Info,
        "hint" => Severity::Hint,
        _ => return None,
    };
    Some((word.len(), Ink::Color(crate::colors::diagnostic(severity))))
}

/// The row for a symbol whose text is `text`, `cols` cells wide, its
/// characters numbered in `accents` picked out: indented two cells for
/// each of `depth`, its first `name_len` bytes, its name, drawn plainly,
/// and the rest quieter.
fn symbol_row(text: &str, accents: &[u32], (depth, name_len): (u8, usize), cols: usize) -> Row {
    let indent = "  ".repeat(usize::from(depth).min(cols / 4));
    let shown = format!("{indent}{text}");
    let inks: Vec<Ink> = (indent.chars().map(|_| Ink::Plain))
        .chain(
            (text.char_indices().zip(0_u32..)).map(|((at, _), char_index)| {
                if accents.binary_search(&char_index).is_ok() {
                    Ink::Accent
                } else if at < name_len {
                    Ink::Plain
                } else {
                    Ink::Muted
                }
            }),
        )
        .collect();
    view::fit(&shown, &inks, cols, false)
}

/// What a symbol is, by its kind as LSP numbers them (`SymbolKind`), as
/// the finder says it.
fn symbol_kind(kind: u8) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type parameter",
        _ => "symbol",
    }
}

#[cfg(test)]
mod tests;
